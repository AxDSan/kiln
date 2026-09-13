#![allow(unused_imports)]
//! Part of the backend lowering, split out of `lib.rs` (Phase 1). This is the
//! same `impl Lowerer<'_>` methods as before, moved verbatim; no behaviour
//! change.
use crate::*;
use kiln_ir::registry::{ComponentKind, DllSig};
use kiln_ir::sema::resolve_ret;
use kiln_ir::{
    BinOp, BitOp, CallConv, CmpOp, Component, Elem, Expr, LogicalOp, Module, Registry, Signature,
    TargetInfo, Ty,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

impl Lowerer<'_> {
    /// Reserve a stack slot, emitted at the top of the function.
    pub(crate) fn alloca(&mut self, ty: Ty) -> String {
        let slot = format!("%v{}", self.locals);
        self.locals += 1;
        self.allocas
            .push(format!("  {slot} = alloca {}\n", llvm_ty(ty)));
        slot
    }

    /// Reserve a scratch slot for a command call's slot ABI. It is named from
    /// the temporary counter rather than the local one, because it belongs to
    /// an expression rather than to anything the program named — but it is
    /// emitted in `entry:` like every other slot, so a call inside a loop
    /// reserves its space once rather than once per turn.
    pub(crate) fn alloca_temp(&mut self, ty: &str) -> String {
        let slot = self.fresh();
        self.allocas.push(format!("  {slot} = alloca {ty}\n"));
        slot
    }

    /// Reserve `size` raw bytes on the stack — the flat storage of a c-record.
    /// `[size x i8]` gives one whole object with byte-addressable fields, so a
    /// field GEP is a plain byte offset and the layout is ours, not LLVM's.
    pub(crate) fn alloca_bytes(&mut self, size: i64) -> String {
        let slot = format!("%v{}", self.locals);
        self.locals += 1;
        self.allocas
            .push(format!("  {slot} = alloca [{size} x i8]\n"));
        slot
    }

    /// The `sizeof` of a c-record, from the one layout function every consumer
    /// shares.
    pub(crate) fn c_record_size(&self, rec: &str) -> Result<i64, LowerError> {
        let def = self.reg.record(rec).ok_or_else(|| LowerError {
            msg: format!("unknown record `{rec}`"),
        })?;
        let (_, size, _) = def
            .c_layout(self.reg, self.machine)
            .ok_or_else(|| LowerError {
                msg: format!("c-record `{rec}` has a field with no C layout"),
            })?;
        Ok(size)
    }

    /// The byte offset and *declared* field type (a `byte` stays `byte` here —
    /// the load/store needs its real width; `surface` maps it to `int` only for
    /// the resulting `Val`) of one field of a c-record.
    pub(crate) fn c_field(&self, rec: &str, field: &str) -> Result<(i64, Ty), LowerError> {
        let def = self.reg.record(rec).ok_or_else(|| LowerError {
            msg: format!("unknown record `{rec}`"),
        })?;
        let (pos, ty) = def.field(field).ok_or_else(|| LowerError {
            msg: format!("c-record `{rec}` has no field `{field}`"),
        })?;
        let (offsets, _, _) = def
            .c_layout(self.reg, self.machine)
            .ok_or_else(|| LowerError {
                msg: format!("c-record `{rec}` has a field with no C layout"),
            })?;
        Ok((offsets[pos - 1], ty))
    }

    /// A pointer to a field inside a c-record's flat storage: `base` + offset.
    /// A zero offset is `base` itself, which reads better and is the same
    /// address.
    pub(crate) fn c_field_ptr(&mut self, base: &str, offset: i64) -> String {
        if offset == 0 {
            return base.to_string();
        }
        let p = self.fresh();
        writeln!(
            self.body,
            "  {p} = getelementptr inbounds i8, ptr {base}, i64 {offset}"
        )
        .unwrap();
        p
    }

    /// The module variables the collector needs to know about: the
    /// pointer-typed ones, sorted so the table and the call agree and so the
    /// generated IR is stable between builds. A text or an array held only in
    /// a module variable is reachable from nowhere else — no stack frame holds
    /// it between the subroutines that touch it — so without this table the
    /// first collection would free it.
    pub(crate) fn gc_root_symbols(&self) -> Vec<String> {
        let mut names: Vec<&String> = self
            .globals
            .iter()
            .filter(|(_, ty)| ty.is_pointer())
            .map(|(n, _)| n)
            .collect();
        names.sort();
        names.iter().map(|n| global_symbol(n)).collect()
    }

    /// `{"a": 1}` — one empty dictionary, then one store per pair, for the
    /// reason an array literal is built the same way: a value may be any
    /// expression, so there is nothing constant to initialise from.
    pub(crate) fn eval_dict_lit(
        &mut self,
        value: Elem,
        pairs: &[(Expr, Expr)],
    ) -> Result<Val, LowerError> {
        self.aggr_used.insert("kn_dict_new");
        let d = self.fresh();
        writeln!(
            self.body,
            "  {d} = call ptr @kn_dict_new(i32 {})",
            value.ty().sdt_tag()
        )
        .unwrap();
        for (key, val) in pairs {
            let k = self.eval(key)?;
            if k.ty != Ty::Text {
                return err(format!(
                    "a dictionary is keyed by text, got {}",
                    k.ty.as_str()
                ));
            }
            let v = self.eval_hinted(val, Some(value.ty()))?;
            if v.ty != value.ty() {
                return err(format!(
                    "every value in a dictionary has one type: expected {}, got {}",
                    value.as_str(),
                    v.ty.as_str()
                ));
            }
            let raw = self.emit_arg_i64(&v);
            self.aggr_used.insert("kn_dict_put");
            writeln!(
                self.body,
                "  call void @kn_dict_put(ptr {d}, ptr {}, i64 {raw})",
                k.operand
            )
            .unwrap();
        }
        Ok(Val {
            ty: Ty::Dict(value),
            operand: d,
        })
    }

    /// `point(x: 1, y: 2)` — one allocation of the declared width, then one
    /// store per field. A field is written by POSITION: the declaration order
    /// is the layout, so no field name reaches the shipped binary.
    pub(crate) fn eval_record_lit(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
    ) -> Result<Val, LowerError> {
        let def = self.reg.record(name).cloned().ok_or_else(|| LowerError {
            msg: format!("unknown record `{name}`"),
        })?;
        self.aggr_used.insert("kn_rec_new");
        let r = self.fresh();
        writeln!(
            self.body,
            "  {r} = call ptr @kn_rec_new(i32 {})",
            def.fields.len()
        )
        .unwrap();
        for (fname, value) in fields {
            let (pos, want) = def.field(fname).ok_or_else(|| LowerError {
                msg: format!("record `{name}` has no field `{fname}`"),
            })?;
            let v = self.eval_hinted(value, Some(want))?;
            if v.ty != want {
                return err(format!(
                    "record `{name}` field `{fname}` is {}, got {}",
                    want.as_str(),
                    v.ty.as_str()
                ));
            }
            let raw = self.emit_arg_i64(&v);
            self.aggr_used.insert("kn_rec_set");
            writeln!(
                self.body,
                "  call void @kn_rec_set(ptr {r}, i32 {pos}, i64 {raw})"
            )
            .unwrap();
        }
        Ok(Val {
            ty: Ty::Record(kiln_ir::intern(name)),
            operand: r,
        })
    }

    /// Read one field of an already-lowered record. `base` is the record's
    /// value: a heap pointer for a plain record, the flat storage's address for
    /// a c-record (a c-record `Var` yields its own address, not a load).
    pub(crate) fn emit_field_read(
        &mut self,
        rec: &str,
        base: &Val,
        field: &str,
    ) -> Result<Val, LowerError> {
        if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
            return self.emit_c_field_read(rec, &base.operand, field);
        }
        let def = self.reg.record(rec).cloned().ok_or_else(|| LowerError {
            msg: format!("unknown record `{rec}`"),
        })?;
        let (pos, ty) = def.field(field).ok_or_else(|| LowerError {
            msg: format!("record `{rec}` has no field `{field}`"),
        })?;
        self.aggr_used.insert("kn_rec_get");
        let raw = self.fresh();
        writeln!(
            self.body,
            "  {raw} = call i64 @kn_rec_get(ptr {}, i32 {pos})",
            base.operand
        )
        .unwrap();
        let res = self.emit_ret_from_i64(ty, &raw);
        Ok(Val { ty, operand: res })
    }

    /// Read one field of a c-record from its flat storage: a GEP to the field's
    /// byte offset, then a load of the field's real width.
    pub(crate) fn emit_c_field_read(
        &mut self,
        rec: &str,
        base: &str,
        field: &str,
    ) -> Result<Val, LowerError> {
        let (offset, fty) = self.c_field(rec, field)?;
        let fp = self.c_field_ptr(base, offset);
        Ok(self.c_load(&fp, fty))
    }

    /// Load one C-layout value at `fp`. The result is the field's *surface*
    /// type — a `byte` and an `int16` come back as an `int`, a `float` as a
    /// `double` — which is the type the language reads and writes it as.
    ///
    /// A nested c-record and an inline array are the exception: there is no
    /// value to load, so the result IS the address, typed as the field. That
    /// is the same rule a c-record `Var` already follows, which is what lets
    /// `r.pt.x` and `r.rgb[3]` chain through this one function.
    pub(crate) fn c_load(&mut self, fp: &str, fty: Ty) -> Val {
        match fty {
            Ty::Record(_) | Ty::CArray(_) => Val {
                ty: fty,
                operand: fp.to_string(),
            },
            // A byte is one `i8` widened to the `int` it reads as, unsigned so
            // 200 is 200 and not -56.
            Ty::Byte => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i8, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i8 {raw} to i32").unwrap();
                Val {
                    ty: Ty::Int,
                    operand: t,
                }
            }
            // Widened unsigned for the same reason: the field this exists for is
            // a Win32 `WORD`, and 0xFFFF there means 65535, not -1.
            Ty::Int16 => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i16, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i16 {raw} to i32").unwrap();
                Val {
                    ty: Ty::Int,
                    operand: t,
                }
            }
            // The struct holds a 4-byte float; the language has one floating
            // type, so widen on the way out.
            Ty::Float => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load float, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = fpext float {raw} to double").unwrap();
                Val {
                    ty: Ty::Double,
                    operand: t,
                }
            }
            // C truth is any non-zero int, and a C API fills a `BOOL` field with
            // whatever its flag arithmetic produced (`7`, `0x100`), not always
            // `1`. Normalise to 0/1 so `f.on = true` and `not f.on` are right —
            // the same normalisation a returned `dll` bool gets.
            Ty::Bool => {
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i32, ptr {fp}").unwrap();
                let nz = self.fresh();
                writeln!(self.body, "  {nz} = icmp ne i32 {raw}, 0").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i1 {nz} to i32").unwrap();
                Val {
                    ty: Ty::Bool,
                    operand: t,
                }
            }
            // A `char*` a C API wrote into the struct is borrowed and outlives
            // nothing in particular, so copy it into a managed text exactly as a
            // `dll` that returns text does — `kn_dll_text` also turns a NULL
            // field into the empty text.
            Ty::Text => {
                self.needs_dll_text = true;
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load ptr, ptr {fp}").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = call ptr @kn_dll_text(ptr {raw})").unwrap();
                Val {
                    ty: Ty::Text,
                    operand: t,
                }
            }
            _ => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = load {}, ptr {fp}", llvm_ty(fty)).unwrap();
                Val {
                    ty: fty.surface(),
                    operand: t,
                }
            }
        }
    }

    /// Write one field of a c-record into its flat storage: evaluate the value
    /// against the field's surface type, then store it at the field's offset in
    /// the field's real width.
    pub(crate) fn emit_c_field_write(
        &mut self,
        rec: &str,
        base: &str,
        field: &str,
        value: &kiln_ir::Expr,
    ) -> Result<(), LowerError> {
        let (offset, fty) = self.c_field(rec, field)?;
        let fp = self.c_field_ptr(base, offset);
        self.c_store(
            &fp,
            fty,
            value,
            &format!("c-record `{rec}` field `{field}`"),
        )
    }

    /// Store one C-layout value at `fp`, narrowing to the field's real width.
    /// `what` names the destination for the type error.
    pub(crate) fn c_store(
        &mut self,
        fp: &str,
        fty: Ty,
        value: &kiln_ir::Expr,
        what: &str,
    ) -> Result<(), LowerError> {
        if matches!(fty, Ty::Record(_) | Ty::CArray(_)) {
            return err(format!(
                "{what} is a whole nested struct or inline array — set its parts, or copy \
                 bytes through `address of`"
            ));
        }
        let want = fty.surface();
        let v = self.eval_hinted(value, Some(want))?;
        if v.ty != want {
            return err(format!(
                "{what} is {}, cannot store {}",
                want.as_str(),
                v.ty.as_str()
            ));
        }
        match fty {
            // The `int` value narrows to the one byte the field holds; the top
            // 24 bits are the author's to keep in range, as with `ptr_write_byte`.
            Ty::Byte => {
                let b = self.fresh();
                writeln!(self.body, "  {b} = trunc i32 {} to i8", v.operand).unwrap();
                writeln!(self.body, "  store i8 {b}, ptr {fp}").unwrap();
            }
            // The low 16 bits, for the same reason.
            Ty::Int16 => {
                let b = self.fresh();
                writeln!(self.body, "  {b} = trunc i32 {} to i16", v.operand).unwrap();
                writeln!(self.body, "  store i16 {b}, ptr {fp}").unwrap();
            }
            // A `double` narrows to the 4-byte float the struct holds — the
            // rounding C's own `float x = d;` does.
            Ty::Float => {
                let f = self.fresh();
                writeln!(self.body, "  {f} = fptrunc double {} to float", v.operand).unwrap();
                writeln!(self.body, "  store float {f}, ptr {fp}").unwrap();
            }
            // The stored `char*` is borrowed: it is valid while the text that
            // backs it is, exactly like `ptr_of_text`. A runtime-produced empty
            // text is NULL, so store a pointer to `""` instead, matching how a
            // text argument crosses to a `dll`.
            Ty::Text => {
                let empty = self.cstr("");
                let isnull = self.fresh();
                writeln!(self.body, "  {isnull} = icmp eq ptr {}, null", v.operand).unwrap();
                let sel = self.fresh();
                writeln!(
                    self.body,
                    "  {sel} = select i1 {isnull}, ptr {empty}, ptr {}",
                    v.operand
                )
                .unwrap();
                writeln!(self.body, "  store ptr {sel}, ptr {fp}").unwrap();
            }
            _ => {
                writeln!(
                    self.body,
                    "  store {} {}, ptr {fp}",
                    llvm_ty(fty),
                    v.operand
                )
                .unwrap();
            }
        }
        Ok(())
    }

    /// The address of a *place* inside a c-record's flat storage, and the place's
    /// declared C field type: `r`, `r.pt`, `r.pt.x`, `r.rgb[3]`, however deep.
    ///
    /// One walker for every path in the language — the read of `r.pt.x`, the
    /// write to it, `address of r.rgb`, and an element read — so a chained GEP
    /// is computed in exactly one place and the three cannot disagree about an
    /// offset.
    pub(crate) fn c_place_ptr(&mut self, place: &Expr) -> Result<(String, Ty), LowerError> {
        match place {
            Expr::Var(name) => match self.vars.get(name).cloned() {
                Some((slot, ty @ Ty::Record(rec)))
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) =>
                {
                    Ok((slot, ty))
                }
                _ => err(format!(
                    "`{name}` is not a c-record local — a path reaches into a c-record's own \
                     storage and nothing else"
                )),
            },
            // `r.pt` on its own arrives as a property read; past the first step
            // the parser builds `Field`. Both are one step into a record.
            Expr::GetProperty {
                component,
                property,
            } => {
                let base = Expr::Var(component.clone());
                self.c_place_step(&base, property)
            }
            Expr::Field { base, name } => self.c_place_step(base, name),
            Expr::Index { base, index } => {
                let (bp, bty) = self.c_place_ptr(base)?;
                let Ty::CArray(a) = bty else {
                    return err(format!(
                        "`{}` is not an inline array — only an array field is indexed inside a \
                         c-record",
                        bty.as_str()
                    ));
                };
                let (esize, _) = kiln_ir::c_field_size_align(a.elem, self.reg, self.machine)
                    .ok_or_else(|| LowerError {
                        msg: format!("`{}` has no C layout", a.elem.as_str()),
                    })?;
                // Positions count from 1, so element `k` starts `(k-1)*esize`
                // bytes in. A literal index folds to that constant; anything
                // else is a runtime GEP with no bounds check, exactly as every
                // other `ptr` operation is.
                let p = match self.const_index(index) {
                    Some(k) => self.c_field_ptr(&bp, (k - 1) * esize),
                    None => {
                        let iv = self.eval(index)?;
                        if iv.ty != Ty::Int {
                            return err(format!(
                                "an index counts with `int` values, got {}",
                                iv.ty.as_str()
                            ));
                        }
                        let zero = self.fresh();
                        writeln!(self.body, "  {zero} = sub i32 {}, 1", iv.operand).unwrap();
                        let wide = self.fresh();
                        writeln!(self.body, "  {wide} = sext i32 {zero} to i64").unwrap();
                        let off = self.fresh();
                        writeln!(self.body, "  {off} = mul i64 {wide}, {esize}").unwrap();
                        let g = self.fresh();
                        writeln!(
                            self.body,
                            "  {g} = getelementptr inbounds i8, ptr {bp}, i64 {off}"
                        )
                        .unwrap();
                        g
                    }
                };
                Ok((p, a.elem))
            }
            other => err(format!("{other:?} is not a place inside a c-record")),
        }
    }

    /// The value of an index the compiler can see: a literal, or a `const` that
    /// stands for one. Folding the constant here is what keeps `r.rgb[LIMIT]` a
    /// plain constant offset rather than an address computed at run time.
    pub(crate) fn const_index(&self, index: &Expr) -> Option<i64> {
        match index {
            Expr::IntLit(k) => Some(*k),
            Expr::Var(n) if !self.vars.contains_key(n) => {
                match self.reg.const_(n).map(|c| &c.value) {
                    Some(Expr::IntLit(k)) => Some(*k),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Whether a place expression bottoms out in a c-record local — the test
    /// that decides whether `xs[i]` is a struct's inline array or a runtime
    /// array. It answers from the variable table alone, so it never emits.
    pub(crate) fn rooted_in_c_record(&self, place: &Expr) -> bool {
        match place {
            Expr::Var(name)
            | Expr::GetProperty {
                component: name, ..
            } => {
                matches!(self.vars.get(name), Some((_, Ty::Record(rec)))
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false))
            }
            Expr::Field { base, .. } | Expr::Index { base, .. } => self.rooted_in_c_record(base),
            _ => false,
        }
    }

    /// One `.field` step of a place walk.
    pub(crate) fn c_place_step(
        &mut self,
        base: &Expr,
        field: &str,
    ) -> Result<(String, Ty), LowerError> {
        let (bp, bty) = self.c_place_ptr(base)?;
        let Ty::Record(rec) = bty else {
            return err(format!(
                "`.{field}` reads a field, and {} has none",
                bty.as_str()
            ));
        };
        let (offset, fty) = self.c_field(rec, field)?;
        Ok((self.c_field_ptr(&bp, offset), fty))
    }

    /// The declared type of a variable, local first then module-level — the
    /// order the checker resolves a name in.
    pub(crate) fn var_ty(&self, name: &str) -> Option<Ty> {
        self.vars
            .get(name)
            .map(|(_, t)| *t)
            .or_else(|| self.globals.get(name).copied())
    }

    /// `[a, b, c]` — one allocation of the right length, then one store per
    /// element. Building it through `kn_ary_set` rather than a constant
    /// initializer is what lets an element be any expression.
    pub(crate) fn eval_array_lit(&mut self, elem: Elem, items: &[Expr]) -> Result<Val, LowerError> {
        self.aggr_used.insert("kn_ary_new");
        let arr = self.fresh();
        writeln!(
            self.body,
            "  {arr} = call ptr @kn_ary_new(i32 {}, i32 {})",
            elem.ty().sdt_tag(),
            items.len()
        )
        .unwrap();
        for (i, item) in items.iter().enumerate() {
            let v = self.eval(item)?;
            if v.ty != elem.ty() {
                return err(format!(
                    "every element of a list has one type: expected {}, got {}",
                    elem.as_str(),
                    v.ty.as_str()
                ));
            }
            let raw = self.emit_arg_i64(&v);
            self.aggr_used.insert("kn_ary_set");
            // `enumerate` counts from 0 and the store counts from 1. Getting
            // this wrong writes every element one place low and drops the
            // first, which looks like a broken literal rather than an
            // off-by-one.
            let pos = i + 1;
            writeln!(
                self.body,
                "  call void @kn_ary_set(ptr {arr}, i32 {pos}, i64 {raw})"
            )
            .unwrap();
        }
        Ok(Val {
            ty: Ty::Array(elem),
            operand: arr,
        })
    }

    /// Read one element. The bounds check lives in the runtime helper, which
    /// reports through the error slot — reading past the end must never reach
    /// whatever is next in memory.
    pub(crate) fn eval_index(&mut self, base: &Expr, index: &Expr) -> Result<Val, LowerError> {
        let b = self.eval(base)?;
        let i = self.eval(index)?;
        // A dictionary is subscripted by key. The miss is reported by the
        // runtime through the error slot, so a lookup that finds nothing is
        // still a value the caller can hold.
        if let Ty::Dict(value) = b.ty {
            if i.ty != Ty::Text {
                return err(format!(
                    "a dictionary is keyed by text, got {}",
                    i.ty.as_str()
                ));
            }
            self.aggr_used.insert("kn_dict_at");
            let raw = self.fresh();
            writeln!(
                self.body,
                "  {raw} = call i64 @kn_dict_at(ptr {}, ptr {})",
                b.operand, i.operand
            )
            .unwrap();
            let res = self.emit_ret_from_i64(value.ty(), &raw);
            return Ok(Val {
                ty: value.ty(),
                operand: res,
            });
        }
        if i.ty != Ty::Int {
            return err(format!(
                "an index counts with `int` values, got {}",
                i.ty.as_str()
            ));
        }
        match b.ty {
            Ty::Bytes => {
                self.aggr_used.insert("kn_bin_at");
                let t = self.fresh();
                writeln!(
                    self.body,
                    "  {t} = call i32 @kn_bin_at(ptr {}, i32 {})",
                    b.operand, i.operand
                )
                .unwrap();
                Ok(Val {
                    ty: Ty::Int,
                    operand: t,
                })
            }
            Ty::Array(elem) => {
                self.aggr_used.insert("kn_ary_get");
                let raw = self.fresh();
                writeln!(
                    self.body,
                    "  {raw} = call i64 @kn_ary_get(ptr {}, i32 {})",
                    b.operand, i.operand
                )
                .unwrap();
                let res = self.emit_ret_from_i64(elem.ty(), &raw);
                Ok(Val {
                    ty: elem.ty(),
                    operand: res,
                })
            }
            other => err(format!("{} is not something you can index", other.as_str())),
        }
    }
}
