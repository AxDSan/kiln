//! KIR → textual LLVM IR.
//!
//! A straight typed-tree walk: KIR has no names to resolve and no sugar to
//! expand, so this module makes no choices the front end has not already made.
//! Every local is alloca-backed (as the 1.x backend does); `opt`'s mem2reg
//! reconstructs SSA when optimisation is on.
//!
//! The slot-ABI marshalling for [`Call::Command`] mirrors the 1.x backend
//! (`backend/src/lower/calls.rs`) instruction for instruction, so a command
//! called from KIR reaches the runtime identically.

use crate::*;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// How a variable declaration is spelled in the debug information.
///
/// LLVM 19 introduced debug *records* and LLVM 21 removed the `llvm.dbg.*`
/// intrinsics they replaced, so no single spelling is accepted across the
/// supported range: a module written for clang 21 does not parse under the
/// clang 18 that Ubuntu 24.04 still ships. Debug information is on by default,
/// so getting this wrong is a build that fails outright rather than a build
/// without debug info. The caller asks the toolchain and chooses — the same
/// choice the 1.x backend's `DebugFormat` makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DebugSpelling {
    /// `#dbg_declare(...)` — LLVM 19 and later.
    Records,
    /// `call void @llvm.dbg.declare(...)` — LLVM 18 and earlier.
    Intrinsics,
}

/// Emit a whole module as a `.ll` string, with the modern debug records.
pub fn emit(m: &Module) -> String {
    emit_with(m, DebugSpelling::Records)
}

/// Emit a whole module, naming the debug spelling the assembler will accept.
pub fn emit_with(m: &Module, debug_spelling: DebugSpelling) -> String {
    let mut e = Emit {
        m,
        debug_spelling,
        strings: Vec::new(),
        externs: BTreeSet::new(),
        extra_globals: Vec::new(),
        debug: m
            .source
            .as_deref()
            .map(|s| crate::debug::Debug::new(s, concat!("Kiln 2 ", env!("CARGO_PKG_VERSION")))),
    };
    let mut funcs = String::new();
    for f in &m.funcs {
        funcs.push_str(&e.func(f));
        funcs.push('\n');
    }
    let entry = m.entry.map(|id| e.entry(id));
    e.assemble(&funcs, entry.as_deref())
}

struct Emit<'a> {
    m: &'a Module,
    /// Which spelling of a debug declaration the assembler accepts.
    debug_spelling: DebugSpelling,
    strings: Vec<String>,
    externs: BTreeSet<String>,
    /// Globals the emitter itself needs — a lazily resolved function's cache.
    extra_globals: Vec<String>,
    /// Present when the module names the file it came from.
    debug: Option<crate::debug::Debug>,
}

impl Emit<'_> {
    fn ty(&self) -> &TyTable {
        &self.m.types
    }

    /// The module variables the collector must treat as roots, sorted so the
    /// table and the call agree and the generated IR is stable between builds.
    ///
    /// A string, a list or a record held only in a module variable is
    /// reachable from nowhere else — no stack frame holds it between the
    /// functions that touch it — so without this table the first collection
    /// frees it. Handed over only in an executable, and only when the collector
    /// is linked: a library has no moment before its host could allocate, and
    /// the libc-only path never collects at all, which is 1.x's rule too.
    fn gc_roots(&self) -> Vec<&str> {
        if self.m.entry.is_none() || self.m.allocator != Allocator::Runtime {
            return Vec::new();
        }
        let mut names: Vec<&str> = self
            .m
            .globals
            .iter()
            .filter(|g| g.is_gc_root)
            .map(|g| g.name.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    // ─── module assembly ────────────────────────────────────────────────────

    fn assemble(&self, funcs: &str, entry: Option<&str>) -> String {
        let mut out = String::new();
        writeln!(
            out,
            "; Kiln-generated LLVM IR — module `{}` (KIR)",
            self.m.name
        )
        .unwrap();
        writeln!(out, "; Do not edit; regenerate from the .kiln source.\n").unwrap();
        writeln!(out, "%Slot = type {{ i32, i32, i64 }}\n").unwrap();

        // Named struct types for C-layout records.
        for r in &self.m.records {
            if let Layout::C { .. } = r.layout {
                let fields: Vec<String> = r
                    .fields
                    .iter()
                    .map(|f| self.ty().llvm_in_place(f.ty, |rid| self.m.record(rid).name.clone()))
                    .collect();
                writeln!(out, "%rec.{} = type {{ {} }}", r.name, fields.join(", ")).unwrap();
            }
        }
        if self
            .m
            .records
            .iter()
            .any(|r| matches!(r.layout, Layout::C { .. }))
        {
            out.push('\n');
        }

        // Globals.
        for g in &self.m.globals {
            writeln!(
                out,
                "@{} = internal global {} {}",
                g.name,
                self.ty().llvm(g.ty),
                self.ty().zero(g.ty)
            )
            .unwrap();
        }
        for g in &self.extra_globals {
            writeln!(out, "{g}").unwrap();
        }
        if !self.m.globals.is_empty() || !self.extra_globals.is_empty() {
            out.push('\n');
        }

        // The collector's root table: the addresses of the pointer-typed module
        // variables, so a collection cannot free what only a global names.
        let roots = self.gc_roots();
        if !roots.is_empty() {
            let items: Vec<String> = roots.iter().map(|r| format!("ptr @{r}")).collect();
            writeln!(
                out,
                "@kn_gc_roots = internal global [{} x ptr] [{}]\n",
                roots.len(),
                items.join(", ")
            )
            .unwrap();
        }

        // String constants.
        for (i, s) in self.strings.iter().enumerate() {
            let bytes = s.len() + 1;
            writeln!(
                out,
                "@.str{i} = private unnamed_addr constant [{bytes} x i8] c\"{}\\00\"",
                encode_llvm_string(s)
            )
            .unwrap();
        }
        if !self.strings.is_empty() {
            out.push('\n');
        }

        // Declarations.
        for d in &self.externs {
            writeln!(out, "{d}").unwrap();
        }
        if !roots.is_empty() {
            writeln!(out, "declare void @kn_gc_set_roots(ptr, i32)").unwrap();
        }
        if !self.externs.is_empty() || !roots.is_empty() {
            out.push('\n');
        }

        out.push_str(funcs);
        if let Some(entry) = entry {
            out.push_str(entry);
        }
        if let Some(d) = &self.debug {
            if !d.is_empty() {
                out.push_str(&d.render());
            }
        }
        out
    }

    fn entry(&mut self, id: FuncId) -> String {
        let f = self.m.func(id);
        let mut out = String::new();
        writeln!(out, "define i32 @ECodeStart() {{").unwrap();
        writeln!(out, "entry:").unwrap();
        // The collector's roots, before anything can allocate: a static
        // field's initial value runs inside the module entry function and may
        // itself allocate.
        let roots = self.gc_roots();
        if !roots.is_empty() {
            writeln!(
                out,
                "  call void @kn_gc_set_roots(ptr @kn_gc_roots, i32 {})",
                roots.len()
            )
            .unwrap();
        }
        // Call the module entry function, discard/forward its result.
        if f.ret == TyTable::VOID {
            writeln!(out, "  call void @{}()", f.symbol).unwrap();
            writeln!(out, "  ret i32 0").unwrap();
        } else {
            let rt = self.ty().llvm(f.ret);
            writeln!(out, "  %rc = call {rt} @{}()", f.symbol).unwrap();
            if f.ret == TyTable::I32 {
                writeln!(out, "  ret i32 %rc").unwrap();
            } else {
                writeln!(out, "  ret i32 0").unwrap();
            }
        }
        writeln!(out, "}}").unwrap();
        out
    }

    // ─── functions ──────────────────────────────────────────────────────────

    fn func(&mut self, f: &Func) -> String {
        let mut fe = FnEmit::new(self, f);
        fe.run()
    }

    fn intern_str(&mut self, s: &str) -> usize {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i;
        }
        self.strings.push(s.to_string());
        self.strings.len() - 1
    }
}

/// Per-function emission state.
struct FnEmit<'a, 'b> {
    e: &'a mut Emit<'b>,
    f: &'a Func,
    /// The subprogram this function's instructions belong to.
    scope: Option<usize>,
    /// The location the next instruction carries.
    loc: Option<usize>,
    head: String,
    body: String,
    tmp: usize,
    label: usize,
    /// `(continue target, break target)` for enclosing loops, innermost last.
    loops: Vec<(String, String)>,
}

/// A lowered value: its LLVM operand and KIR type.
struct V {
    op: String,
    ty: TyId,
}

impl<'a, 'b> FnEmit<'a, 'b> {
    fn new(e: &'a mut Emit<'b>, f: &'a Func) -> FnEmit<'a, 'b> {
        FnEmit {
            e,
            f,
            scope: None,
            loc: None,
            head: String::new(),
            body: String::new(),
            tmp: 0,
            label: 0,
            loops: Vec::new(),
        }
    }

    fn tt(&self) -> &TyTable {
        &self.e.m.types
    }

    fn fresh(&mut self) -> String {
        let t = format!("%t{}", self.tmp);
        self.tmp += 1;
        t
    }

    fn fresh_label(&mut self, tag: &str) -> String {
        let l = format!("{tag}{}", self.label);
        self.label += 1;
        l
    }

    fn local_ptr(&self, id: LocalId) -> String {
        format!("%l.{}", id.0)
    }

    /// Attach the current source location to every instruction written since
    /// the last line marker. Doing it as a pass over the finished text keeps
    /// the two hundred places that write an instruction from having to know
    /// about debug information at all.
    fn tag_line(&mut self, from: usize) {
        let Some(loc) = self.loc else { return };
        let tail = self.body.split_off(from);
        for line in tail.lines() {
            let t = line.trim_start();
            // Labels and terminator-less lines take no location; a plain
            // instruction does.
            if t.is_empty() || t.ends_with(':') {
                self.body.push_str(line);
                self.body.push('\n');
                continue;
            }
            self.body.push_str(line);
            self.body.push_str(&format!(", !dbg !{loc}\n"));
        }
    }

    fn run(&mut self) -> String {
        // Signature.
        let cc = match self.f.conv {
            CallConv::Stdcall | CallConv::System if self.e.m.target.windows && self.e.m.target.ptr_bits == 32 => {
                "x86_stdcallcc "
            }
            _ => "",
        };
        let ret = self.tt().llvm(self.f.ret);
        let params: Vec<String> = self
            .f
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{} %arg{i}", self.tt().llvm(p.ty)))
            .collect();
        let linkage = match self.f.linkage {
            Linkage::Internal => "internal ",
            Linkage::Exported => "",
        };

        // A function the user wrote gets a subprogram; a synthetic one — a
        // thunk, a lifted lambda — has no source position worth stopping in.
        if !self.f.synthetic {
            if let Some(d) = self.e.debug.as_mut() {
                let sp = d.subprogram_in(&self.f.symbol, self.f.line, self.f.file.as_deref());
                self.scope = Some(sp);
                // Start at the declaration line rather than at no location.
                // LLVM discards a module's debug information entirely if a
                // call in a function that has some carries no `!dbg` — so an
                // instruction emitted before the first line marker (a
                // prologue, an initialiser) would silently cost the whole
                // program its debuggability.
                let line = self.f.line;
                self.loc = Some(d.location(line, sp));
            }
        }

        // Allocas for every local; spill parameters into their slots.
        for (i, l) in self.f.locals.iter().enumerate() {
            let lt = self.tt().llvm(l.ty);
            writeln!(
                self.head,
                "  {} = alloca {lt}",
                self.local_ptr(LocalId(i as u32))
            )
            .unwrap();
        }
        for (i, l) in self.f.locals.iter().enumerate() {
            if let Some(argn) = l.is_arg {
                let lt = self.tt().llvm(l.ty);
                writeln!(
                    self.head,
                    "  store {lt} %arg{argn}, ptr {}",
                    self.local_ptr(LocalId(i as u32))
                )
                .unwrap();
            }
        }

        // Describe each named local against the slot that holds it, so a
        // debugger can print it. Compiler-invented names carry a `$` and are
        // left out: they are machinery, not the user's variables.
        if let Some(scope) = self.scope {
            let described: Vec<(usize, String, TyId, Option<usize>, usize)> = self
                .f
                .locals
                .iter()
                .enumerate()
                .filter(|(_, l)| !l.name.starts_with('$') && !l.name.is_empty())
                .map(|(i, l)| {
                    (
                        i,
                        l.name.clone(),
                        l.ty,
                        l.is_arg.map(|a| a + 1),
                        if l.span.line > 0 {
                            l.span.line
                        } else {
                            self.f.line
                        },
                    )
                })
                .collect();
            for (i, name, ty, arg, line) in described {
                // A record is described by its fields, so a debugger prints it
                // as `{W = 3, H = 4}` rather than as an address. The third
                // element says whether the value is held behind a pointer (a
                // record) or in the slot itself (an optional).
                let described_shape: Option<(
                    String,
                    u64,
                    bool,
                    Vec<(String, String, u32, &'static str, u64)>,
                )> = match *self.tt().kind(ty) {
                    TyKind::Record(rid) => {
                        let rec = self.e.m.record(rid);
                        match &rec.layout {
                            Layout::C { size, offsets, .. } => {
                                let members = rec
                                    .fields
                                    .iter()
                                    .zip(offsets.iter())
                                    .map(|(f, off)| {
                                        let (tn, bits, enc) = describe_ty(self.tt(), f.ty);
                                        (f.name.clone(), tn, bits, enc, *off as u64 * 8)
                                    })
                                    .collect();
                                Some((rec.name.clone(), *size as u64 * 8, true, members))
                            }
                            Layout::Managed => None,
                        }
                    }
                    // `T?` is `{ value, present }` held in the slot, not behind
                    // a pointer. Describing it as a flat struct is what lets a
                    // debugger tell an absent value from a zero and read the
                    // value it does hold; without it the slot is one unnamed
                    // pointer, which is also how a `string` is described, so an
                    // `int?` read as an address and a `string?` as text.
                    TyKind::Optional(inner) => {
                        let present_at = size_bytes(self.tt(), inner);
                        let (vtn, vbits, venc) = optional_value_desc(self.tt(), inner);
                        let members = vec![
                            ("value".to_string(), vtn, vbits, venc, 0u64),
                            (
                                "present".to_string(),
                                "bool".to_string(),
                                8u32,
                                "DW_ATE_boolean",
                                present_at * 8,
                            ),
                        ];
                        let name = format!("$Opt<{},{}>", self.tt().llvm(inner), venc);
                        let total = size_bytes(self.tt(), ty);
                        Some((name, total * 8, false, members))
                    }
                    _ => None,
                };
                let (tname, bits, enc) = describe_ty(self.tt(), ty);
                let (var, loc) = {
                    let Some(d) = self.e.debug.as_mut() else {
                        break;
                    };
                    let tn = match described_shape {
                        Some((rname, size_bits, by_pointer, members)) => {
                            let described_members: Vec<(String, usize, u64, u64)> = members
                                .into_iter()
                                .map(|(mname, tn, mbits, menc, off)| {
                                    let node = d.basic_type(&tn, mbits, menc);
                                    (mname, node, mbits as u64, off)
                                })
                                .collect();
                            let composite = d.record_type(&rname, size_bits, &described_members);
                            if by_pointer {
                                d.pointer_to(composite)
                            } else {
                                composite
                            }
                        }
                        None => d.basic_type(&tname, bits, enc),
                    };
                    let var = d.local(&name, scope, line, tn, arg);
                    let loc = d.location(line, scope);
                    (var, loc)
                };
                let slot = self.local_ptr(LocalId(i as u32));
                match self.e.debug_spelling {
                    // A debug *record*, which is not an instruction and needs
                    // no declaration.
                    DebugSpelling::Records => {
                        writeln!(
                            self.head,
                            "    #dbg_declare(ptr {slot}, !{var}, !DIExpression(), !{loc})"
                        )
                        .unwrap();
                    }
                    // The intrinsic, whose declaration the module must carry.
                    // LLVM 19 and later auto-upgrade this call to a record.
                    DebugSpelling::Intrinsics => {
                        self.e.externs.insert(
                            "declare void @llvm.dbg.declare(metadata, metadata, metadata)".into(),
                        );
                        writeln!(
                            self.head,
                            "  call void @llvm.dbg.declare(metadata ptr {slot}, metadata !{var}, \
                             metadata !DIExpression()), !dbg !{loc}"
                        )
                        .unwrap();
                    }
                }
            }
        }

        let fell_through = self.stmts(&self.f.body.clone());
        // A function that runs off the end needs a terminator.
        if fell_through {
            if self.f.ret == TyTable::VOID {
                writeln!(self.body, "  ret void").unwrap();
            } else {
                writeln!(
                    self.body,
                    "  ret {} {}",
                    self.tt().llvm(self.f.ret),
                    self.tt().zero(self.f.ret)
                )
                .unwrap();
            }
        }

        let mut out = String::new();
        let dbg = match self.scope {
            Some(n) => format!(" !dbg !{n}"),
            None => String::new(),
        };
        writeln!(
            out,
            "define {linkage}{cc}{ret} @{}({}){dbg} {{",
            self.f.symbol,
            params.join(", ")
        )
        .unwrap();
        writeln!(out, "entry:").unwrap();
        out.push_str(&self.head);
        out.push_str(&self.body);
        writeln!(out, "}}").unwrap();
        out
    }

    // ─── statements ───────────────────────────────────────────────────────
    // Returns true if control falls through (no terminator emitted).

    fn stmts(&mut self, stmts: &[Stmt]) -> bool {
        let mut fell_through = true;
        for s in stmts {
            if !fell_through {
                break; // dead code after a terminator
            }
            fell_through = self.stmt(s);
        }
        fell_through
    }

    fn stmt(&mut self, s: &Stmt) -> bool {
        // A marker only moves the current location; it emits nothing.
        if let Stmt::Line(line) = s {
            if let (Some(scope), Some(d)) = (self.scope, self.e.debug.as_mut()) {
                self.loc = Some(d.location(*line, scope));
            }
            return true;
        }
        let from = self.body.len();
        let fell_through = self.stmt_inner(s);
        if self.loc.is_some() {
            self.tag_line(from);
        }
        fell_through
    }

    fn stmt_inner(&mut self, s: &Stmt) -> bool {
        match s {
            // `stmt` consumes a line marker before dispatching here.
            Stmt::Line(_) => true,
            Stmt::Let { local, value } => {
                let v = self.expr(value);
                let lt = self.tt().llvm(self.f.locals[local.0 as usize].ty);
                writeln!(
                    self.body,
                    "  store {lt} {}, ptr {}",
                    v.op,
                    self.local_ptr(*local)
                )
                .unwrap();
                true
            }
            Stmt::Assign { place, value } => {
                let v = self.expr(value);
                let (ptr, ty) = self.place_ptr(place);
                let lt = self.tt().llvm(ty);
                writeln!(self.body, "  store {lt} {}, ptr {ptr}", v.op).unwrap();
                true
            }
            Stmt::Expr(e) => {
                // A call in statement position may be void; only a value
                // position requires a result.
                match e {
                    Expr::Call(c) => {
                        self.call(c);
                    }
                    other => {
                        self.expr(other);
                    }
                }
                true
            }
            Stmt::Return(v) => {
                match v {
                    None => writeln!(self.body, "  ret void").unwrap(),
                    Some(e) => {
                        let val = self.expr(e);
                        writeln!(self.body, "  ret {} {}", self.tt().llvm(val.ty), val.op).unwrap();
                    }
                }
                false
            }
            Stmt::If { cond, then, els } => self.emit_if(cond, then, els),
            Stmt::Loop { body } => self.emit_loop(body),
            Stmt::Break => {
                let target = self.loops.last().expect("break outside loop").1.clone();
                writeln!(self.body, "  br label %{target}").unwrap();
                false
            }
            Stmt::Continue => {
                let target = self.loops.last().expect("continue outside loop").0.clone();
                writeln!(self.body, "  br label %{target}").unwrap();
                false
            }
        }
    }

    fn emit_if(&mut self, cond: &Expr, then: &[Stmt], els: &[Stmt]) -> bool {
        let c = self.expr(cond);
        let then_l = self.fresh_label("then");
        let else_l = self.fresh_label("else");
        let end_l = self.fresh_label("endif");
        writeln!(
            self.body,
            "  br i1 {}, label %{then_l}, label %{else_l}",
            c.op
        )
        .unwrap();

        writeln!(self.body, "{then_l}:").unwrap();
        let then_ft = self.stmts(then);
        if then_ft {
            writeln!(self.body, "  br label %{end_l}").unwrap();
        }

        writeln!(self.body, "{else_l}:").unwrap();
        let else_ft = self.stmts(els);
        if else_ft {
            writeln!(self.body, "  br label %{end_l}").unwrap();
        }

        if then_ft || else_ft {
            writeln!(self.body, "{end_l}:").unwrap();
            true
        } else {
            false
        }
    }

    fn emit_loop(&mut self, body: &[Stmt]) -> bool {
        let top = self.fresh_label("loop");
        let end = self.fresh_label("loopend");
        writeln!(self.body, "  br label %{top}").unwrap();
        writeln!(self.body, "{top}:").unwrap();
        self.loops.push((top.clone(), end.clone()));
        let ft = self.stmts(body);
        self.loops.pop();
        if ft {
            writeln!(self.body, "  br label %{top}").unwrap();
        }
        // The break target is reachable whenever a `Break` was emitted; assume
        // so and give it a home. Emitting the label is always valid.
        writeln!(self.body, "{end}:").unwrap();
        true
    }

    // ─── places ─────────────────────────────────────────────────────────────

    fn place_ptr(&mut self, p: &Place) -> (String, TyId) {
        match p {
            Place::Local(id) => (self.local_ptr(*id), self.f.locals[id.0 as usize].ty),
            Place::Global(id) => {
                let g = self.e.m.global(*id);
                (format!("@{}", g.name), g.ty)
            }
            Place::Field(base, idx) => {
                let b = self.expr(base);
                self.field_ptr(&b, *idx)
            }
            Place::Index(base, idx) => {
                let b = self.expr(base);
                let i = self.expr(idx);
                if let TyKind::Inline { elem, .. } = *self.tt().kind(b.ty) {
                    return (self.inline_elem_ptr(&b, &i), elem);
                }
                let elem = match self.tt().kind(b.ty) {
                    TyKind::Array(e) => *e,
                    k => panic!("index assignment on non-array {k:?}"),
                };
                let p = self.elem_ptr(&b, &i, elem);
                (p, elem)
            }
        }
    }

    /// Address of element `i` of the contiguous buffer `b` holds. The index is
    /// already 0-based: the front end subtracts one from Kiln's 1-based
    /// positions before it gets here.
    fn elem_ptr(&mut self, b: &V, i: &V, elem: TyId) -> String {
        let idx = if self.tt().llvm(i.ty) == "i64" {
            i.op.clone()
        } else {
            let t = self.fresh();
            writeln!(
                self.body,
                "  {t} = sext {} {} to i64",
                self.tt().llvm(i.ty),
                i.op
            )
            .unwrap();
            t
        };
        let p = self.fresh();
        writeln!(
            self.body,
            "  {p} = getelementptr {}, ptr {}, i64 {idx}",
            self.tt().llvm(elem),
            b.op
        )
        .unwrap();
        p
    }

    /// Address of element `i` (0-based) of an inline array whose address is `b`.
    fn inline_elem_ptr(&mut self, b: &V, i: &V) -> String {
        let ty = self.tt().llvm_in_place(b.ty, |rid| self.e.m.record(rid).name.clone());
        let idx = if self.tt().llvm(i.ty) == "i64" {
            i.op.clone()
        } else {
            let t = self.fresh();
            writeln!(self.body, "  {t} = sext {} {} to i64", self.tt().llvm(i.ty), i.op).unwrap();
            t
        };
        let p = self.fresh();
        writeln!(self.body, "  {p} = getelementptr {ty}, ptr {}, i64 0, i64 {idx}", b.op).unwrap();
        p
    }

    /// Pointer to field `idx` of a C-layout record value `b`, and the field type.
    fn field_ptr(&mut self, b: &V, idx: usize) -> (String, TyId) {
        let rid = match self.tt().kind(b.ty) {
            TyKind::Record(rid) => *rid,
            k => panic!("field access on non-record {k:?}"),
        };
        let rec = self.e.m.record(rid);
        let name = rec.name.clone();
        let fty = rec.fields[idx].ty;
        let p = self.fresh();
        writeln!(
            self.body,
            "  {p} = getelementptr %rec.{name}, ptr {}, i32 0, i32 {idx}",
            b.op
        )
        .unwrap();
        (p, fty)
    }

    // ─── expressions ──────────────────────────────────────────────────────

    fn expr(&mut self, e: &Expr) -> V {
        match e {
            Expr::Int(v, ty) => V {
                op: v.to_string(),
                ty: *ty,
            },
            Expr::Float(v, ty) => V {
                op: float_lit(*v, *ty == TyTable::F32),
                ty: *ty,
            },
            Expr::Bool(b) => V {
                op: if *b { "true".into() } else { "false".into() },
                ty: TyTable::BOOL,
            },
            Expr::Str(s) => {
                let id = self.e.intern_str(s);
                V {
                    op: format!("@.str{id}"),
                    ty: TyTable::STR,
                }
            }
            Expr::Null(ty) => V {
                op: "null".into(),
                ty: *ty,
            },
            Expr::Local(id) => {
                let ty = self.f.locals[id.0 as usize].ty;
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = load {}, ptr {}",
                    self.tt().llvm(ty),
                    self.local_ptr(*id)
                )
                .unwrap();
                V { op: t, ty }
            }
            Expr::Global(id) => {
                let g = self.e.m.global(*id);
                let (name, ty) = (g.name.clone(), g.ty);
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = load {}, ptr @{name}",
                    self.tt().llvm(ty)
                )
                .unwrap();
                V { op: t, ty }
            }
            Expr::Field(base, idx) => {
                let b = self.expr(base);
                let (ptr, fty) = self.field_ptr(&b, *idx);
                // Storage held in place is not loaded: its address is the value
                // — a nested record is that record, an inline array is itself.
                if let TyKind::Inline { elem, count } = *self.tt().kind(fty) {
                    return V { op: ptr, ty: if count == 0 { elem } else { fty } };
                }
                let t = self.fresh();
                writeln!(self.body, "  {t} = load {}, ptr {ptr}", self.tt().llvm(fty)).unwrap();
                V { op: t, ty: fty }
            }
            Expr::Index(base, idx) => {
                let b = self.expr(base);
                let i = self.expr(idx);
                if let TyKind::Inline { elem, .. } = *self.tt().kind(b.ty) {
                    let p = self.inline_elem_ptr(&b, &i);
                    if matches!(self.tt().kind(elem), TyKind::Record(_)) {
                        return V { op: p, ty: elem };
                    }
                    let t = self.fresh();
                    writeln!(self.body, "  {t} = load {}, ptr {p}", self.tt().llvm(elem)).unwrap();
                    return V { op: t, ty: elem };
                }
                let elem = match self.tt().kind(b.ty) {
                    TyKind::Array(e) => *e,
                    k => panic!("index read on non-array {k:?}"),
                };
                let p = self.elem_ptr(&b, &i, elem);
                let t = self.fresh();
                writeln!(self.body, "  {t} = load {}, ptr {p}", self.tt().llvm(elem)).unwrap();
                V { op: t, ty: elem }
            }
            Expr::Bin(op, a, b, ty) => self.bin(*op, a, b, *ty),
            Expr::Not(x) => {
                let v = self.expr(x);
                let t = self.fresh();
                writeln!(self.body, "  {t} = xor i1 {}, true", v.op).unwrap();
                V {
                    op: t,
                    ty: TyTable::BOOL,
                }
            }
            Expr::Neg(x, ty) => {
                let v = self.expr(x);
                let t = self.fresh();
                if self.tt().is_float(*ty) {
                    writeln!(self.body, "  {t} = fneg {} {}", self.tt().llvm(*ty), v.op).unwrap();
                } else {
                    writeln!(self.body, "  {t} = sub {} 0, {}", self.tt().llvm(*ty), v.op).unwrap();
                }
                V { op: t, ty: *ty }
            }
            Expr::Cast { value, to } => self.cast(value, *to),
            Expr::MakeRecord(rid, fields) => self.make_record(*rid, fields),
            Expr::MakeArray(elem, items) => self.make_array(*elem, items),
            Expr::MakeTuple(items) => self.make_tuple(items),
            Expr::MakeOptional(inner, val) => self.make_optional(*inner, val.as_deref()),
            Expr::OptionalHasValue(x) => {
                let v = self.expr(x);
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = extractvalue {} {}, 1",
                    self.tt().llvm(v.ty),
                    v.op
                )
                .unwrap();
                V {
                    op: t,
                    ty: TyTable::BOOL,
                }
            }
            Expr::OptionalGet(x) => {
                let v = self.expr(x);
                let inner = match self.tt().kind(v.ty) {
                    TyKind::Optional(t) => *t,
                    _ => panic!("OptionalGet on non-optional"),
                };
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = extractvalue {} {}, 0",
                    self.tt().llvm(v.ty),
                    v.op
                )
                .unwrap();
                V { op: t, ty: inner }
            }
            Expr::FuncPtr(f) => {
                let sym = self.e.m.func(*f).symbol.clone();
                V {
                    op: format!("@{sym}"),
                    ty: TyTable::PTR,
                }
            }
            Expr::FuncValue { fn_ptr, env } => {
                let f = self.expr(fn_ptr);
                let en = self.expr(env);
                let t0 = self.fresh();
                writeln!(
                    self.body,
                    "  {t0} = insertvalue {{ ptr, ptr }} undef, ptr {}, 0",
                    f.op
                )
                .unwrap();
                let t1 = self.fresh();
                writeln!(
                    self.body,
                    "  {t1} = insertvalue {{ ptr, ptr }} {t0}, ptr {}, 1",
                    en.op
                )
                .unwrap();
                // The caller knows the signature; the pair itself is untyped.
                V {
                    op: t1,
                    ty: TyTable::PTR,
                }
            }
            Expr::ElemPtr(base, idx) => {
                let b = self.expr(base);
                let i = self.expr(idx);
                let elem = match self.tt().kind(b.ty) {
                    TyKind::Array(e) => *e,
                    k => panic!("ElemPtr on non-array {k:?}"),
                };
                let p = self.elem_ptr(&b, &i, elem);
                V {
                    op: p,
                    ty: TyTable::PTR,
                }
            }
            Expr::MakeClosure { func, env } => self.make_closure(*func, env),
            Expr::Call(c) => self.call(c).expect("call in value position returns void"),
        }
    }

    fn bin(&mut self, op: BinOp, a: &Expr, b: &Expr, ty: TyId) -> V {
        let l = self.expr(a);
        let r = self.expr(b);
        let lt = self.tt().llvm(ty);
        let float = self.tt().is_float(ty);
        let uns = self.tt().is_unsigned(ty);
        let t = self.fresh();

        let (instr, result_bool) = match op {
            BinOp::Add => (if float { "fadd" } else { "add" }.to_string(), false),
            BinOp::Sub => (if float { "fsub" } else { "sub" }.to_string(), false),
            BinOp::Mul => (if float { "fmul" } else { "mul" }.to_string(), false),
            BinOp::Div => (
                if float {
                    "fdiv"
                } else if uns {
                    "udiv"
                } else {
                    "sdiv"
                }
                .to_string(),
                false,
            ),
            BinOp::Rem => (
                if float {
                    "frem"
                } else if uns {
                    "urem"
                } else {
                    "srem"
                }
                .to_string(),
                false,
            ),
            BinOp::And => ("and".to_string(), false),
            BinOp::Or => ("or".to_string(), false),
            BinOp::Xor => ("xor".to_string(), false),
            BinOp::Shl => ("shl".to_string(), false),
            BinOp::Shr => (if uns { "lshr" } else { "ashr" }.to_string(), false),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let pred = cmp_pred(op, float, uns);
                let cmp = if float { "fcmp" } else { "icmp" };
                writeln!(self.body, "  {t} = {cmp} {pred} {lt} {}, {}", l.op, r.op).unwrap();
                return V {
                    op: t,
                    ty: TyTable::BOOL,
                };
            }
        };
        let _ = result_bool;
        writeln!(self.body, "  {t} = {instr} {lt} {}, {}", l.op, r.op).unwrap();
        V { op: t, ty }
    }

    fn cast(&mut self, value: &Expr, to: TyId) -> V {
        let v = self.expr(value);
        if v.ty == to {
            return v;
        }
        let from_ll = self.tt().llvm(v.ty);
        let to_ll = self.tt().llvm(to);
        let from_float = self.tt().is_float(v.ty);
        let to_float = self.tt().is_float(to);
        let from_ptr = self.tt().is_pointer(v.ty);
        let to_ptr = self.tt().is_pointer(to);
        let t = self.fresh();

        let instr = if from_ptr && to_ptr {
            "bitcast"
        } else if from_ptr && !to_ptr {
            "ptrtoint"
        } else if !from_ptr && to_ptr {
            "inttoptr"
        } else if from_float && to_float {
            let fb = float_bits(v.ty);
            let tb = float_bits(to);
            if tb > fb {
                "fpext"
            } else {
                "fptrunc"
            }
        } else if from_float && !to_float {
            if self.tt().is_unsigned(to) {
                "fptoui"
            } else {
                "fptosi"
            }
        } else if !from_float && to_float {
            if self.tt().is_unsigned(v.ty) {
                "uitofp"
            } else {
                "sitofp"
            }
        } else {
            // int → int
            let fb = int_bits(self.tt(), v.ty);
            let tb = int_bits(self.tt(), to);
            if tb < fb {
                "trunc"
            } else if tb > fb {
                // A bool is 0 or 1: widening it must zero-extend, or `true`
                // becomes -1 rather than 1.
                if self.tt().is_unsigned(v.ty) || matches!(self.tt().kind(v.ty), TyKind::Bool) {
                    "zext"
                } else {
                    "sext"
                }
            } else {
                // same width, different signedness — a no-op reinterpret.
                return V { op: v.op, ty: to };
            }
        };
        writeln!(self.body, "  {t} = {instr} {from_ll} {} to {to_ll}", v.op).unwrap();
        V { op: t, ty: to }
    }

    fn make_record(&mut self, rid: RecordId, fields: &[Expr]) -> V {
        let rec = self.e.m.record(rid);
        let name = rec.name.clone();
        let (size, field_tys): (i64, Vec<TyId>) = match &rec.layout {
            Layout::C { size, .. } => (*size, rec.fields.iter().map(|f| f.ty).collect()),
            Layout::Managed => unimplemented!("managed MakeRecord: kn_rec_new, later phase"),
        };
        // A record is heap-allocated. With the collector linked the block must
        // come from it, or it is never traced and never reclaimed.
        let base = self.fresh();
        match self.e.m.allocator {
            Allocator::Runtime => {
                self.e
                    .externs
                    .insert("declare ptr @kn_notify(i32, ptr, ptr)".to_string());
                let sz = self.fresh();
                writeln!(self.body, "  {sz} = inttoptr i64 {size} to ptr").unwrap();
                writeln!(
                    self.body,
                    "  {base} = call ptr @kn_notify(i32 1, ptr {sz}, ptr null)"
                )
                .unwrap();
            }
            Allocator::Libc => {
                self.e
                    .externs
                    .insert(format!("declare ptr @malloc(i{})", self.e.m.target.ptr_bits));
                writeln!(self.body, "  {base} = call ptr @malloc(i{} {size})", self.e.m.target.ptr_bits).unwrap();
            }
        }
        // Storage held in place has no value to store: it starts as zero bytes,
        // as a C struct from `calloc` would.
        let has_inline = field_tys
            .iter()
            .any(|t| matches!(self.tt().kind(*t), TyKind::Inline { .. }));
        if has_inline {
            self.e
                .externs
                .insert("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)".into());
            writeln!(
                self.body,
                "  call void @llvm.memset.p0.i64(ptr {base}, i8 0, i64 {size}, i1 false)"
            )
            .unwrap();
        }
        let vals: Vec<V> = fields.iter().map(|f| self.expr(f)).collect();
        for (i, v) in vals.iter().enumerate() {
            if matches!(self.tt().kind(field_tys[i]), TyKind::Inline { .. }) {
                continue;
            }
            let p = self.fresh();
            writeln!(
                self.body,
                "  {p} = getelementptr %rec.{name}, ptr {base}, i32 0, i32 {i}"
            )
            .unwrap();
            writeln!(
                self.body,
                "  store {} {}, ptr {p}",
                self.tt().llvm(field_tys[i]),
                v.op
            )
            .unwrap();
        }
        V {
            op: base,
            ty: self.tt_intern(TyKind::Record(rid)),
        }
    }

    fn make_array(&mut self, elem: TyId, items: &[Expr]) -> V {
        // kn_ary_new(count, elem_tag) then kn_ary_set per element.
        self.e
            .externs
            .insert("declare ptr @kn_ary_new(i32, i32)".into());
        self.e
            .externs
            .insert("declare void @kn_ary_set(ptr, i32, i64)".into());
        let tag = self.tt().sdt_tag(elem);
        let base = self.fresh();
        // `kn_ary_new(tag, len)` — the tag comes first. Swapped, an empty
        // literal asks for an array of `tag` elements with no element type,
        // which fails far from here: a command reading it binds nine
        // uninitialised parameters and blames its own arguments.
        writeln!(
            self.body,
            "  {base} = call ptr @kn_ary_new(i32 {tag}, i32 {})",
            items.len()
        )
        .unwrap();
        for (i, it) in items.iter().enumerate() {
            let v = self.expr(it);
            let raw = self.to_i64(&v);
            // Positions count from 1, as everywhere else in Kiln.
            let pos = i + 1;
            writeln!(
                self.body,
                "  call void @kn_ary_set(ptr {base}, i32 {pos}, i64 {raw})"
            )
            .unwrap();
        }
        V {
            op: base,
            ty: self.tt_intern(TyKind::Array(elem)),
        }
    }

    fn make_tuple(&mut self, items: &[Expr]) -> V {
        let vals: Vec<V> = items.iter().map(|it| self.expr(it)).collect();
        let tys: Vec<TyId> = vals.iter().map(|v| v.ty).collect();
        let tuple_ty = self.tt_intern(TyKind::Tuple(tys.clone()));
        let ll = self.tt().llvm(tuple_ty);
        let mut cur = "undef".to_string();
        for (i, v) in vals.iter().enumerate() {
            let t = self.fresh();
            writeln!(
                self.body,
                "  {t} = insertvalue {ll} {cur}, {} {}, {i}",
                self.tt().llvm(v.ty),
                v.op
            )
            .unwrap();
            cur = t;
        }
        V {
            op: cur,
            ty: tuple_ty,
        }
    }

    fn make_optional(&mut self, inner: TyId, val: Option<&Expr>) -> V {
        let opt_ty = self.tt_intern(TyKind::Optional(inner));
        let ll = self.tt().llvm(opt_ty);
        let inner_ll = self.tt().llvm(inner);
        match val {
            None => V {
                op: format!("{{ {inner_ll} {}, i1 false }}", self.tt().zero(inner)),
                ty: opt_ty,
            },
            Some(e) => {
                let v = self.expr(e);
                let t0 = self.fresh();
                writeln!(
                    self.body,
                    "  {t0} = insertvalue {ll} undef, {inner_ll} {}, 0",
                    v.op
                )
                .unwrap();
                let t1 = self.fresh();
                writeln!(self.body, "  {t1} = insertvalue {ll} {t0}, i1 true, 1").unwrap();
                V { op: t1, ty: opt_ty }
            }
        }
    }

    fn make_closure(&mut self, func: FuncId, env: &Expr) -> V {
        let f = self.e.m.func(func);
        let sym = f.symbol.clone();
        let params: Vec<TyId> = f.params.iter().skip(1).map(|p| p.ty).collect();
        let ret = f.ret;
        let env_v = self.expr(env);
        let clos_ty = self.tt_intern(TyKind::Func { params, ret });
        let t0 = self.fresh();
        writeln!(
            self.body,
            "  {t0} = insertvalue {{ ptr, ptr }} undef, ptr @{sym}, 0"
        )
        .unwrap();
        let t1 = self.fresh();
        writeln!(
            self.body,
            "  {t1} = insertvalue {{ ptr, ptr }} {t0}, ptr {}, 1",
            env_v.op
        )
        .unwrap();
        V {
            op: t1,
            ty: clos_ty,
        }
    }

    // ─── calls ──────────────────────────────────────────────────────────────
    // Returns None for a void call.

    fn call(&mut self, c: &Call) -> Option<V> {
        match c {
            Call::Direct { func, args } => {
                let f = self.e.m.func(*func);
                let sym = f.symbol.clone();
                let ret = f.ret;
                let ptys: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
                let argv: Vec<String> = args
                    .iter()
                    .zip(&ptys)
                    .map(|(a, pt)| {
                        let v = self.expr(a);
                        format!("{} {}", self.tt().llvm(*pt), v.op)
                    })
                    .collect();
                self.emit_call(&format!("@{sym}"), &argv, ret, "")
            }
            Call::Indirect { callee, args, sig }
                if matches!(self.tt().kind(*sig), TyKind::CFunc { .. }) =>
            {
                // A C function address: no environment, the callee's own
                // convention, and a `char*` result copied as a `[Dll]` one is.
                let (params, ret, conv) = match self.tt().kind(*sig) {
                    TyKind::CFunc { params, ret, conv } => (params.clone(), *ret, *conv),
                    _ => unreachable!(),
                };
                let fp = self.expr(callee);
                let argv: Vec<String> = args
                    .iter()
                    .zip(&params)
                    .map(|(a, pt)| {
                        let v = self.expr(a);
                        format!("{} {}", self.tt().llvm(*pt), v.op)
                    })
                    .collect();
                let cc = match conv {
                    CallConv::Stdcall | CallConv::System if self.e.m.target.windows && self.e.m.target.ptr_bits == 32 => {
                        "x86_stdcallcc "
                    }
                    _ => "",
                };
                if ret == TyTable::STR {
                    self.e.externs.insert("declare ptr @kn_dll_text(ptr)".into());
                    let raw = self.fresh();
                    writeln!(self.body, "  {raw} = call {cc}ptr {}({})", fp.op, argv.join(", ")).unwrap();
                    let out = self.fresh();
                    writeln!(self.body, "  {out} = call ptr @kn_dll_text(ptr {raw})").unwrap();
                    return Some(V { op: out, ty: TyTable::STR });
                }
                self.emit_call(&fp.op, &argv, ret, cc)
            }
            Call::Indirect { callee, args, sig } => {
                let clos = self.expr(callee);
                let (params, ret) = match self.tt().kind(*sig) {
                    TyKind::Func { params, ret } => (params.clone(), *ret),
                    _ => panic!("indirect call on non-Func"),
                };
                let fnp = self.fresh();
                writeln!(
                    self.body,
                    "  {fnp} = extractvalue {{ ptr, ptr }} {}, 0",
                    clos.op
                )
                .unwrap();
                let envp = self.fresh();
                writeln!(
                    self.body,
                    "  {envp} = extractvalue {{ ptr, ptr }} {}, 1",
                    clos.op
                )
                .unwrap();
                let mut argv = vec![format!("ptr {envp}")];
                for (a, pt) in args.iter().zip(&params) {
                    let v = self.expr(a);
                    argv.push(format!("{} {}", self.tt().llvm(*pt), v.op));
                }
                self.emit_call(&fnp, &argv, ret, "")
            }
            Call::Dll {
                library,
                symbol,
                conv,
                args,
                arg_tys,
                ret,
                varargs,
            } if !*varargs && self.e.m.foreign_libraries.contains(library) => {
                // A library the program is not linked against: resolved at the
                // first call and cached, so a program whose library is missing
                // still starts and says which symbol it could not find when it
                // gets there — 1.x's `dll`, exactly.
                let argv: Vec<String> = args
                    .iter()
                    .zip(arg_tys)
                    .map(|(a, t)| {
                        let v = self.expr(a);
                        format!("{} {}", self.tt().llvm(*t), v.op)
                    })
                    .collect();
                let n = self.e.extra_globals.len();
                let cache = format!("@dll.cache.{n}");
                self.e.extra_globals.push(format!("{cache} = internal global ptr null"));
                self.e
                    .externs
                    .insert("declare ptr @kn_dll_get(ptr, ptr, ptr)".into());
                let lib = self.e.intern_str(library);
                let sym = self.e.intern_str(symbol);
                let fp = self.fresh();
                writeln!(
                    self.body,
                    "  {fp} = call ptr @kn_dll_get(ptr {cache}, ptr @.str{lib}, ptr @.str{sym})"
                )
                .unwrap();
                let cc = match conv {
                    CallConv::Stdcall | CallConv::System if self.e.m.target.windows && self.e.m.target.ptr_bits == 32 => {
                        "x86_stdcallcc "
                    }
                    _ => "",
                };
                // A `char*` the library still owns is copied into a runtime
                // string, and NULL becomes "".
                if *ret == TyTable::STR {
                    self.e.externs.insert("declare ptr @kn_dll_text(ptr)".into());
                    let raw = self.fresh();
                    writeln!(self.body, "  {raw} = call {cc}ptr {fp}({})", argv.join(", ")).unwrap();
                    let out = self.fresh();
                    writeln!(self.body, "  {out} = call ptr @kn_dll_text(ptr {raw})").unwrap();
                    return Some(V { op: out, ty: TyTable::STR });
                }
                self.emit_call(&fp, &argv, *ret, cc)
            }
            Call::Dll {
                library: _,
                symbol,
                conv,
                args,
                arg_tys,
                ret,
                varargs,
            } => {
                // Direct declared extern (libc / already-linked). The cached
                // kn_dll_get path is a later phase; declaring the symbol covers
                // printf/malloc-style fixtures and any statically linked import.
                // `arg_tys` are the FIXED parameters; a varargs call may pass
                // more, each typed by its own evaluated type.
                let argv: Vec<String> = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let v = self.expr(a);
                        let lt = if i < arg_tys.len() {
                            self.tt().llvm(arg_tys[i])
                        } else {
                            self.tt().llvm(v.ty)
                        };
                        format!("{lt} {}", v.op)
                    })
                    .collect();
                let ret_ll = if *ret == TyTable::VOID {
                    "void".to_string()
                } else {
                    self.tt().llvm(*ret)
                };
                let sig_params: Vec<String> = arg_tys.iter().map(|t| self.tt().llvm(*t)).collect();
                // The call site needs the explicit function type; the declare
                // uses the ordinary `ret @name(params, ...)` form.
                let sig = if *varargs {
                    format!("{ret_ll} ({}, ...)", sig_params.join(", "))
                } else {
                    format!("{ret_ll} ({})", sig_params.join(", "))
                };
                let decl_params = if *varargs {
                    let mut p = sig_params.clone();
                    p.push("...".into());
                    p.join(", ")
                } else {
                    sig_params.join(", ")
                };
                self.e
                    .externs
                    .insert(format!("declare {ret_ll} @{symbol}({decl_params})"));
                let cc = match conv {
                    CallConv::Stdcall | CallConv::System
                        if self.e.m.target.windows && self.e.m.target.ptr_bits == 32 =>
                    {
                        "x86_stdcallcc "
                    }
                    _ => "",
                };
                // A varargs callee must be called through its full signature.
                if *varargs {
                    let t = if *ret == TyTable::VOID {
                        writeln!(self.body, "  call {cc}{sig} @{symbol}({})", argv.join(", "))
                            .unwrap();
                        return None;
                    } else {
                        let t = self.fresh();
                        writeln!(
                            self.body,
                            "  {t} = call {cc}{sig} @{symbol}({})",
                            argv.join(", ")
                        )
                        .unwrap();
                        t
                    };
                    return Some(V { op: t, ty: *ret });
                }
                self.emit_call(&format!("@{symbol}"), &argv, *ret, cc)
            }
            Call::Command {
                symbol,
                args,
                arg_slots,
                ret,
            } => Some(self.command(symbol, args, arg_slots, *ret)),
        }
    }

    fn emit_call(&mut self, callee: &str, argv: &[String], ret: TyId, cc: &str) -> Option<V> {
        if ret == TyTable::VOID {
            writeln!(self.body, "  call {cc}void {callee}({})", argv.join(", ")).unwrap();
            None
        } else {
            let t = self.fresh();
            writeln!(
                self.body,
                "  {t} = call {cc}{} {callee}({})",
                self.tt().llvm(ret),
                argv.join(", ")
            )
            .unwrap();
            Some(V { op: t, ty: ret })
        }
    }

    /// A support-library command over the slot ABI. Mirrors the 1.x backend.
    fn command(&mut self, symbol: &str, args: &[Expr], slots: &[SlotTy], ret: TyId) -> V {
        let n = args.len();
        let argv = self.fresh_alloca(&format!("[{n} x %Slot]"));
        for (i, (a, slot)) in args.iter().zip(slots).enumerate() {
            let v = self.expr(a);
            let raw = self.to_i64(&v);
            let sp = self.fresh();
            writeln!(
                self.body,
                "  {sp} = getelementptr [{n} x %Slot], ptr {argv}, i64 0, i64 {i}"
            )
            .unwrap();
            let tagp = self.fresh();
            writeln!(
                self.body,
                "  {tagp} = getelementptr %Slot, ptr {sp}, i32 0, i32 0"
            )
            .unwrap();
            writeln!(self.body, "  store i32 {}, ptr {tagp}", slot.tag).unwrap();
            let valp = self.fresh();
            writeln!(
                self.body,
                "  {valp} = getelementptr %Slot, ptr {sp}, i32 0, i32 2"
            )
            .unwrap();
            writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
        }
        let base = self.fresh();
        writeln!(
            self.body,
            "  {base} = getelementptr [{n} x %Slot], ptr {argv}, i64 0, i64 0"
        )
        .unwrap();
        let ret_slot = self.fresh_alloca("%Slot");
        self.e
            .externs
            .insert(format!("declare void @{symbol}(ptr, i32, ptr)"));
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret_slot}, i32 {n}, ptr {base})"
        )
        .unwrap();
        if ret == TyTable::VOID {
            return V {
                op: "0".into(),
                ty: TyTable::VOID,
            };
        }
        let valp = self.fresh();
        writeln!(
            self.body,
            "  {valp} = getelementptr %Slot, ptr {ret_slot}, i32 0, i32 2"
        )
        .unwrap();
        let raw = self.fresh();
        writeln!(self.body, "  {raw} = load i64, ptr {valp}").unwrap();
        self.from_i64(&raw, ret)
    }

    /// Reinterpret a value as the `i64` a slot's value field holds.
    fn to_i64(&mut self, v: &V) -> String {
        match self.tt().kind(v.ty) {
            TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => v.op.clone(),
            TyKind::F64 => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = bitcast double {} to i64", v.op).unwrap();
                t
            }
            TyKind::F32 => {
                let b = self.fresh();
                writeln!(self.body, "  {b} = bitcast float {} to i32", v.op).unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i32 {b} to i64").unwrap();
                t
            }
            k if is_ptr_kind(k) => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = ptrtoint ptr {} to i64", v.op).unwrap();
                t
            }
            TyKind::Bool => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i1 {} to i64", v.op).unwrap();
                t
            }
            _ => {
                // narrower ints: sign- or zero-extend to i64 by signedness
                let ext = if self.tt().is_unsigned(v.ty) {
                    "zext"
                } else {
                    "sext"
                };
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = {ext} {} {} to i64",
                    self.tt().llvm(v.ty),
                    v.op
                )
                .unwrap();
                t
            }
        }
    }

    /// Reinterpret an `i64` from a slot back to `ty`.
    fn from_i64(&mut self, raw: &str, ty: TyId) -> V {
        let op = match self.tt().kind(ty) {
            TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => raw.to_string(),
            TyKind::F64 => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = bitcast i64 {raw} to double").unwrap();
                t
            }
            k if is_ptr_kind(k) => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = inttoptr i64 {raw} to ptr").unwrap();
                t
            }
            TyKind::Bool => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = trunc i64 {raw} to i1").unwrap();
                t
            }
            _ => {
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = trunc i64 {raw} to {}",
                    self.tt().llvm(ty)
                )
                .unwrap();
                t
            }
        };
        V { op, ty }
    }

    fn fresh_alloca(&mut self, ty: &str) -> String {
        let t = format!("%s{}", self.tmp);
        self.tmp += 1;
        writeln!(self.head, "  {t} = alloca {ty}").unwrap();
        t
    }

    fn tt_intern(&mut self, k: TyKind) -> TyId {
        // The module's TyTable is shared and immutable during emit; intern into
        // a lookup that must already hold this kind (front end interned it).
        // For emit-time-created aggregate types (tuples/closures/records built
        // in expressions), the kind is guaranteed present because the front end
        // named the value's type. Fall back to a linear search.
        self.e
            .m
            .types
            .find(&k)
            .unwrap_or_else(|| panic!("type not interned by front end: {k:?}"))
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────

/// How an optional's stored value is described to a debugger.
///
/// A `string` is an unnamed pointer, and walking the characters is the only
/// true reading of one, so it keeps that description. Every other pointer-like
/// inner type — a record, a list, a dictionary, a raw `ptr` — would be read as
/// text under the same unnamed-pointer rule, so it is given the `ptr` name the
/// reader already shows as an address. A scalar keeps its own name and width.
fn optional_value_desc(tt: &TyTable, inner: TyId) -> (String, u32, &'static str) {
    match tt.kind(inner) {
        TyKind::Str => ("ptr".into(), 64, "pointer"),
        _ if tt.is_pointer(inner) => ("ptr".into(), (tt.ptr_bytes() * 8) as u32, "DW_ATE_unsigned"),
        _ => describe_ty(tt, inner),
    }
}

/// How many bytes a value of this type occupies, as LLVM lays it out.
///
/// Only the debug emitter needs this, and only to put an optional's `present`
/// bit where the `{ value, i1 }` the emitter writes actually holds it. The
/// widths are the ones `llvm` spells, measured the way a struct element is:
/// the `i1` follows the value with no padding of its own.
fn size_bytes(tt: &TyTable, ty: TyId) -> u64 {
    match tt.kind(ty) {
        TyKind::Bool | TyKind::I8 | TyKind::U8 => 1,
        TyKind::I16 | TyKind::U16 => 2,
        TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => 4,
        TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint | TyKind::F64 => 8,
        TyKind::Str
        | TyKind::Bytes
        | TyKind::Ptr
        | TyKind::Array(_)
        | TyKind::Dict(..)
        | TyKind::Set(_)
        | TyKind::Record(_)
        | TyKind::CFunc { .. } => tt.ptr_bytes(),
        TyKind::Func { .. } => tt.ptr_bytes() * 2,
        TyKind::Inline { elem, count } => {
            let one = size_bytes(tt, *elem);
            if *count == 0 {
                one
            } else {
                one * u64::from(*count)
            }
        }
        TyKind::Optional(inner) => {
            align_up(size_bytes(tt, *inner) + 1, align_bytes(tt, *inner))
        }
        TyKind::Tuple(elems) => {
            let mut offset = 0;
            let mut alignment = 1;
            for e in elems {
                let a = align_bytes(tt, *e).max(1);
                alignment = alignment.max(a);
                offset = align_up(offset, a) + size_bytes(tt, *e);
            }
            align_up(offset, alignment)
        }
        TyKind::Void => 0,
    }
}

/// The ABI alignment of a value of this type: the largest power of two a
/// struct element of it is placed at. Pointer-like values align to a pointer;
/// a tuple to its widest member.
fn align_bytes(tt: &TyTable, ty: TyId) -> u64 {
    match tt.kind(ty) {
        TyKind::Bool | TyKind::I8 | TyKind::U8 => 1,
        TyKind::I16 | TyKind::U16 => 2,
        TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => 4,
        TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint | TyKind::F64 => 8,
        TyKind::Optional(inner) => align_bytes(tt, *inner),
        TyKind::Tuple(elems) => elems.iter().map(|e| align_bytes(tt, *e)).max().unwrap_or(1),
        _ => tt.ptr_bytes(),
    }
}

fn align_up(n: u64, a: u64) -> u64 {
    if a <= 1 {
        n
    } else {
        n.div_ceil(a) * a
    }
}

/// How a value's type is described to a debugger: its name, its width in bits,
/// and its DWARF encoding. Anything held by pointer is described as one.
fn describe_ty(tt: &TyTable, ty: TyId) -> (String, u32, &'static str) {
    match tt.kind(ty) {
        TyKind::Bool => ("bool".into(), 8, "DW_ATE_boolean"),
        TyKind::I8 => ("sbyte".into(), 8, "DW_ATE_signed"),
        TyKind::U8 => ("byte".into(), 8, "DW_ATE_unsigned"),
        TyKind::I16 => ("short".into(), 16, "DW_ATE_signed"),
        TyKind::U16 => ("ushort".into(), 16, "DW_ATE_unsigned"),
        TyKind::I32 => ("int".into(), 32, "DW_ATE_signed"),
        TyKind::U32 => ("uint".into(), 32, "DW_ATE_unsigned"),
        TyKind::Char => ("char".into(), 32, "DW_ATE_unsigned"),
        TyKind::I64 | TyKind::Nint => ("long".into(), 64, "DW_ATE_signed"),
        TyKind::U64 | TyKind::Nuint => ("ulong".into(), 64, "DW_ATE_unsigned"),
        TyKind::F32 => ("float".into(), 32, "DW_ATE_float"),
        TyKind::F64 => ("double".into(), 64, "DW_ATE_float"),
        _ => ("ptr".into(), 64, "pointer"),
    }
}

fn cmp_pred(op: BinOp, float: bool, uns: bool) -> &'static str {
    match (op, float) {
        (BinOp::Eq, false) => "eq",
        (BinOp::Ne, false) => "ne",
        (BinOp::Lt, false) => {
            if uns {
                "ult"
            } else {
                "slt"
            }
        }
        (BinOp::Le, false) => {
            if uns {
                "ule"
            } else {
                "sle"
            }
        }
        (BinOp::Gt, false) => {
            if uns {
                "ugt"
            } else {
                "sgt"
            }
        }
        (BinOp::Ge, false) => {
            if uns {
                "uge"
            } else {
                "sge"
            }
        }
        (BinOp::Eq, true) => "oeq",
        (BinOp::Ne, true) => "one",
        (BinOp::Lt, true) => "olt",
        (BinOp::Le, true) => "ole",
        (BinOp::Gt, true) => "ogt",
        (BinOp::Ge, true) => "oge",
        _ => unreachable!("non-comparison op in cmp_pred"),
    }
}

fn int_bits(tt: &TyTable, ty: TyId) -> u32 {
    match tt.kind(ty) {
        TyKind::Bool => 1,
        TyKind::I8 | TyKind::U8 => 8,
        TyKind::I16 | TyKind::U16 => 16,
        TyKind::I32 | TyKind::U32 | TyKind::Char => 32,
        TyKind::I64 | TyKind::U64 => 64,
        TyKind::Nint | TyKind::Nuint => tt.ptr_bits,
        _ => panic!("int_bits on non-int"),
    }
}

fn float_bits(ty: TyId) -> u32 {
    match ty {
        TyTable::F32 => 32,
        TyTable::F64 => 64,
        _ => panic!("float_bits on non-float"),
    }
}

/// An LLVM floating-point literal. Hex form so the bits round-trip exactly.
fn float_lit(v: f64, is_f32: bool) -> String {
    let bits = if is_f32 {
        (v as f32 as f64).to_bits()
    } else {
        v.to_bits()
    };
    format!("0x{bits:016X}")
}

fn encode_llvm_string(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'"' | b'\\' => write!(out, "\\{b:02X}").unwrap(),
            0x20..=0x7E => out.push(b as char),
            _ => write!(out, "\\{b:02X}").unwrap(),
        }
    }
    out
}
