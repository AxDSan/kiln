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
    /// The LLVM calling-convention prefix for a call or definition, or an empty
    /// string when the target's default already means it. Emitting
    /// `x86_stdcallcc` on 32-bit Windows is what keeps a `system`-marked call
    /// from corrupting the stack; on every 64-bit target it is empty, so the IR
    /// is byte for byte what it always was.
    pub(crate) fn cc_prefix(&self, conv: Option<CallConv>) -> String {
        match conv.and_then(|c| c.llvm_cc(self.machine)) {
            Some(cc) => format!("{cc} "),
            None => String::new(),
        }
    }

    /// Call a two-argument slot-ABI runtime command and return its result
    /// operand. `call_text_eq` and text `+` are both this call with different
    /// symbols.
    pub(crate) fn call_symbol_2(
        &mut self,
        symbol: &str,
        a: &Val,
        b: &Val,
        ret: Ty,
    ) -> Result<String, LowerError> {
        let argv = self.alloca_temp("[2 x %Slot]");
        for (i, v) in [a, b].iter().enumerate() {
            let raw = self.emit_arg_i64(v);
            let slot = self.fresh();
            writeln!(
                self.body,
                "  {slot} = getelementptr [2 x %Slot], ptr {argv}, i64 0, i64 {i}"
            )
            .unwrap();
            let tagp = self.fresh();
            writeln!(
                self.body,
                "  {tagp} = getelementptr %Slot, ptr {slot}, i32 0, i32 0"
            )
            .unwrap();
            writeln!(self.body, "  store i32 {}, ptr {tagp}", v.ty.sdt_tag()).unwrap();
            let valp = self.fresh();
            writeln!(
                self.body,
                "  {valp} = getelementptr %Slot, ptr {slot}, i32 0, i32 2"
            )
            .unwrap();
            writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
        }
        let base = self.fresh();
        writeln!(
            self.body,
            "  {base} = getelementptr [2 x %Slot], ptr {argv}, i64 0, i64 0"
        )
        .unwrap();
        let ret_slot = self.alloca_temp("%Slot");
        self.used.insert(symbol.to_string());
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret_slot}, i32 2, ptr {base})"
        )
        .unwrap();
        let valp = self.fresh();
        writeln!(
            self.body,
            "  {valp} = getelementptr %Slot, ptr {ret_slot}, i32 0, i32 2"
        )
        .unwrap();
        let raw = self.fresh();
        writeln!(self.body, "  {raw} = load i64, ptr {valp}").unwrap();
        Ok(self.emit_ret_from_i64(ret, &raw))
    }

    /// Call a one-argument slot-ABI runtime command and return its text result.
    pub(crate) fn call_symbol_1(&mut self, symbol: &str, arg: &Val) -> Result<String, LowerError> {
        let raw = self.emit_arg_i64(arg);
        let argv = self.alloca_temp("[1 x %Slot]");
        let slot = self.fresh();
        writeln!(
            self.body,
            "  {slot} = getelementptr [1 x %Slot], ptr {argv}, i64 0, i64 0"
        )
        .unwrap();
        let tagp = self.fresh();
        writeln!(
            self.body,
            "  {tagp} = getelementptr %Slot, ptr {slot}, i32 0, i32 0"
        )
        .unwrap();
        writeln!(self.body, "  store i32 {}, ptr {tagp}", arg.ty.sdt_tag()).unwrap();
        let valp = self.fresh();
        writeln!(
            self.body,
            "  {valp} = getelementptr %Slot, ptr {slot}, i32 0, i32 2"
        )
        .unwrap();
        writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
        let ret = self.alloca_temp("%Slot");
        self.used.insert(symbol.to_string());
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret}, i32 1, ptr {slot})"
        )
        .unwrap();
        let rvalp = self.fresh();
        writeln!(
            self.body,
            "  {rvalp} = getelementptr %Slot, ptr {ret}, i32 0, i32 2"
        )
        .unwrap();
        let rraw = self.fresh();
        writeln!(self.body, "  {rraw} = load i64, ptr {rvalp}").unwrap();
        Ok(self.emit_ret_from_i64(Ty::Text, &rraw))
    }

    /// The declared type of a component property, from the introspected
    /// descriptor (the validator has already proven it exists).
    pub(crate) fn property_ty(&self, component: &str, property: &str) -> Result<Ty, LowerError> {
        let type_name = self
            .component_types
            .get(component)
            .ok_or_else(|| LowerError {
                msg: format!("unknown component `{component}`"),
            })?;
        self.reg
            .component(type_name)
            .and_then(|d| d.property(property))
            .map(|p| p.ty)
            .ok_or_else(|| LowerError {
                msg: format!("`{type_name}` has no property `{property}`"),
            })
    }

    /// Reinterpret a value's operand as the raw `i64` stored in a slot's value
    /// field; returns the operand holding the i64.
    pub(crate) fn emit_arg_i64(&mut self, v: &Val) -> String {
        match v.ty {
            Ty::Int64 => v.operand.clone(),
            Ty::Int | Ty::Bool => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = sext i32 {} to i64", v.operand).unwrap();
                t
            }
            Ty::Double => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = bitcast double {} to i64", v.operand).unwrap();
                t
            }
            _ => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = ptrtoint ptr {} to i64", v.operand).unwrap();
                t
            }
        }
    }

    /// Reinterpret an `i64` loaded from a slot's value field back to `ty`.
    pub(crate) fn emit_ret_from_i64(&mut self, ty: Ty, raw: &str) -> String {
        match ty {
            Ty::Int64 => raw.to_string(),
            Ty::Int | Ty::Bool => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = trunc i64 {raw} to i32").unwrap();
                t
            }
            Ty::Double => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = bitcast i64 {raw} to double").unwrap();
                t
            }
            _ => {
                let t = self.fresh();
                writeln!(self.body, "  {t} = inttoptr i64 {raw} to ptr").unwrap();
                t
            }
        }
    }

    /// Lower a call to a user subroutine: a direct native call, not the slot
    /// ABI. Nothing is marshalled, so recursion and a value return are exactly
    /// what LLVM already does for a C function.
    pub(crate) fn eval_user_call(
        &mut self,
        name: &str,
        sig: &Signature,
        args: &[Expr],
    ) -> Result<Option<Val>, LowerError> {
        if args.len() != sig.params.len() {
            return err(format!(
                "subroutine `{name}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }
        let mut ops = Vec::new();
        for (i, a) in args.iter().enumerate() {
            // Hint each argument with the parameter's declared type, exactly as
            // the command path does — otherwise an `int` literal passed to an
            // `int64` parameter would type-check (the validator hints) and then
            // fail here (this did not). A callback sub taking `int64`/`ptr`
            // parameters is the first thing a later stage calls from user code.
            let v = self.eval_hinted(a, Some(sig.params[i]))?;
            if v.ty != sig.params[i] {
                return err(format!(
                    "subroutine `{name}` argument {} expects {}, got {}",
                    i + 1,
                    sig.params[i].as_str(),
                    v.ty.as_str()
                ));
            }
            ops.push(format!("{} {}", llvm_ty(v.ty), v.operand));
        }
        let arglist = ops.join(", ");
        let symbol = user_symbol(name);
        // An internal call must match the definition's convention: a `system`
        // sub is defined `x86_stdcallcc` on 32-bit, and the verifier refuses a
        // call that disagrees.
        let cc = self.cc_prefix(self.sub_convs.get(name).copied().flatten());
        match sig.ret {
            None => {
                writeln!(self.body, "  call {cc}void @{symbol}({arglist})").unwrap();
                Ok(None)
            }
            Some(t) => {
                let r = self.fresh();
                writeln!(
                    self.body,
                    "  {r} = call {cc}{} @{symbol}({arglist})",
                    llvm_ty(t)
                )
                .unwrap();
                Ok(Some(Val { ty: t, operand: r }))
            }
        }
    }

    /// Lower a call to a foreign function: resolve its symbol through the
    /// runtime loader (cached, so the resolution happens once) and make an
    /// indirect call with the declared C signature.
    ///
    /// This is a plain C call, not the slot ABI — a `dll` names an ordinary C
    /// export, and the whole point is to reach it exactly as C would. Each
    /// argument is already the C representation of its type: an `int` is an
    /// `i32`, a `text` is the `char*` backing it, a `ptr` is the pointer.
    pub(crate) fn eval_dll_call(
        &mut self,
        name: &str,
        dll: &DllSig,
        args: &[Expr],
    ) -> Result<Option<Val>, LowerError> {
        let sig = &dll.sig;
        if args.len() != sig.params.len() {
            return err(format!(
                "foreign function `{name}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }
        let mut ops: Vec<String> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let want = sig.params[i];
            let v = self.eval_hinted(a, Some(want))?;
            if v.ty != want {
                return err(format!(
                    "foreign function `{name}` argument {} expects {}, got {}",
                    i + 1,
                    want.as_str(),
                    v.ty.as_str()
                ));
            }
            ops.push(self.marshal_c_arg(want, &v));
        }

        // Resolve once, cache in a per-declaration global: a call in a loop
        // pays the load-and-lookup cost a single time. `kn_dll_get` reads the
        // cache, resolves and stores it if empty, and returns the address —
        // aborting through `kn_runtime_error` if the library or symbol is
        // missing, so a bad call is a named failure, never a silent 0.
        self.needs_dll_get = true;
        self.dll_cached.insert(name.to_string());
        let cache = dll_cache_symbol(name);
        let lib = self.cstr(&dll.library);
        let sym = self.cstr(&dll.symbol);
        let fp = self.fresh();
        writeln!(
            self.body,
            "  {fp} = call ptr @kn_dll_get(ptr @{cache}, ptr {lib}, ptr {sym})"
        )
        .unwrap();

        // `dll.conv` (stdcall/cdecl/system) is intentionally NOT emitted onto
        // the `call`: every target Kiln builds is 64-bit with a single C
        // convention, so the three markers name the same one and a textual
        // callconv here would only risk perturbing the proven `--os windows`
        // path for no behavioural gain. The marker is carried on the `DllSig`
        // for a future 32-bit backend, which WOULD read it at this point.
        self.emit_c_call(&fp, &ops, sig.ret, dll.conv)
    }

    /// One argument, in the C representation the boundary wants.
    ///
    /// A `text` marshals as the `char*` backing it. That pointer is NULL for a
    /// runtime-produced empty text (an allocation that failed, a
    /// `last_error_text` with nothing to say), and a C function running
    /// `strlen` on NULL would fault — so a null text is handed a pointer to
    /// `""` instead. A literal `""` is already a real pointer, so the common
    /// case takes the fast side of the `select`. Everything else is already its
    /// own C representation: an `int` is an `i32`, a `ptr` is the pointer, and a
    /// c-record `Val` is the address of its flat storage.
    pub(crate) fn marshal_c_arg(&mut self, want: Ty, v: &Val) -> String {
        let operand = if want == Ty::Text {
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
            sel
        } else {
            v.operand.clone()
        };
        format!("{} {}", llvm_ty(want), operand)
    }

    /// Emit the `call` itself, given a callee operand and marshalled arguments,
    /// and bring the C result back into a Kiln value.
    ///
    /// `fp` is a `ptr`-typed operand however it was obtained — the address
    /// `kn_dll_get` resolved for a `dll`, or the run-time pointer a
    /// `call through` was handed. Under opaque pointers those are the same
    /// thing to LLVM, which is why one emitter serves both and a returned
    /// `char*` or C truth is converted in exactly one place.
    pub(crate) fn emit_c_call(
        &mut self,
        fp: &str,
        ops: &[String],
        ret: Option<Ty>,
        conv: Option<CallConv>,
    ) -> Result<Option<Val>, LowerError> {
        let arglist = ops.join(", ");
        // `x86_stdcallcc` on 32-bit Windows, empty on every 64-bit target, so
        // an indirect `dll`/`call through` reaches its callee with the
        // convention the source declared.
        let cc = self.cc_prefix(conv);
        match ret {
            None => {
                writeln!(self.body, "  call {cc}void {fp}({arglist})").unwrap();
                Ok(None)
            }
            Some(Ty::Text) => {
                // The C side returns a `char*` it still owns; copy it into a
                // runtime-owned text so the result lives and is freed like every
                // other text, and a NULL return becomes `""`.
                self.needs_dll_text = true;
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = call {cc}ptr {fp}({arglist})").unwrap();
                let out = self.fresh();
                writeln!(self.body, "  {out} = call ptr @kn_dll_text(ptr {raw})").unwrap();
                Ok(Some(Val {
                    ty: Ty::Text,
                    operand: out,
                }))
            }
            Some(Ty::Bool) => {
                // C truth is any non-zero int; normalise to 0/1 so a returned
                // `bool` compares equal to `true` when it should.
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = call {cc}i32 {fp}({arglist})").unwrap();
                let nz = self.fresh();
                writeln!(self.body, "  {nz} = icmp ne i32 {raw}, 0").unwrap();
                let out = self.fresh();
                writeln!(self.body, "  {out} = zext i1 {nz} to i32").unwrap();
                Ok(Some(Val {
                    ty: Ty::Bool,
                    operand: out,
                }))
            }
            Some(t) => {
                let r = self.fresh();
                writeln!(self.body, "  {r} = call {cc}{} {fp}({arglist})", llvm_ty(t)).unwrap();
                Ok(Some(Val { ty: t, operand: r }))
            }
        }
    }

    /// Lower `call through EXPR(args...): T` — a C call whose callee is a value
    /// rather than a symbol.
    ///
    /// The only difference from a `dll` call is where the address comes from.
    /// There is no `kn_dll_get`, no cache and no library to open: the program
    /// already holds the pointer. Under opaque pointers the `ptr` IS the callee
    /// operand — there is no bitcast to a function type to emit — so the same
    /// `call <ret> %fp(args)` a `dll` produces is what comes out here, which is
    /// why the two share `marshal_c_arg` and `emit_c_call` rather than each
    /// having their own idea of how a `text` crosses.
    ///
    /// Each argument's own type is the parameter type: the call site declared
    /// the signature by writing the expressions, so there is nothing to check
    /// an argument against — the checker has already proven each has a C shape.
    /// The trailing convention marker is emitted the same way a `dll`'s is.
    pub(crate) fn eval_call_through(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        ret: Option<Ty>,
        conv: Option<CallConv>,
    ) -> Result<Option<Val>, LowerError> {
        let target = self.eval(callee)?;
        if target.ty != Ty::Ptr {
            return err(format!(
                "`call through` needs a ptr to call, got {}",
                target.ty.as_str()
            ));
        }
        // The address is evaluated FIRST, before the arguments, so a callee
        // expression with a side effect happens where it is written.
        let fp = target.operand.clone();
        let mut ops: Vec<String> = Vec::new();
        for a in args {
            let v = self.eval(a)?;
            ops.push(self.marshal_c_arg(v.ty, &v));
        }
        self.emit_c_call(&fp, &ops, ret, conv)
    }

    /// Lower a command call via the slot ABI; returns the result if non-void.
    pub(crate) fn eval_call(
        &mut self,
        cmd: &str,
        args: &[Expr],
    ) -> Result<Option<Val>, LowerError> {
        // Commands win the name; the validator has already rejected any user
        // sub that tried to take one.
        if self.reg.get(cmd).is_none() {
            if let Some(sig) = self.reg.sub(cmd).cloned() {
                return self.eval_user_call(cmd, &sig, args);
            }
            if let Some(dll) = self.reg.dll(cmd).cloned() {
                return self.eval_dll_call(cmd, &dll, args);
            }
        }
        let command = self.reg.get(cmd).ok_or_else(|| LowerError {
            msg: format!("unknown command `{cmd}`"),
        })?;
        let sig = command.sig.clone();
        let symbol = command.symbol.clone();

        if args.len() != sig.params.len() {
            return err(format!(
                "command `{cmd}` expects {} argument(s), got {}",
                sig.params.len(),
                args.len()
            ));
        }

        // Lower and type-check each argument first (may emit arithmetic).
        // `AnyArray`/`AnyElem` parameters take their meaning from the array
        // argument this call was given; `resolve_ret` reads the same thing back
        // out for the result, so the two cannot drift.
        let mut arg_vals: Vec<Val> = Vec::new();
        let mut elem: Option<Elem> = None;
        for (i, a) in args.iter().enumerate() {
            let want = match sig.params[i] {
                Ty::AnyArray | Ty::AnyDict => None,
                Ty::AnyElem => elem.map(Elem::ty),
                t => Some(t),
            };
            let v = self.eval_hinted(a, want)?;
            match sig.params[i] {
                Ty::AnyArray => match v.ty.elem() {
                    Some(e) => elem = Some(e),
                    None => {
                        return err(format!(
                            "command `{cmd}` argument {} expects an array, got {}",
                            i + 1,
                            v.ty.as_str()
                        ))
                    }
                },
                Ty::AnyDict => match v.ty.value() {
                    Some(e) => elem = Some(e),
                    None => {
                        return err(format!(
                            "command `{cmd}` argument {} expects a dictionary, got {}",
                            i + 1,
                            v.ty.as_str()
                        ))
                    }
                },
                Ty::AnyElem => {
                    if Some(v.ty) != elem.map(Elem::ty) {
                        return err(format!(
                            "command `{cmd}` argument {} does not match what the collection holds",
                            i + 1
                        ));
                    }
                }
                t if v.ty != t => {
                    return err(format!(
                        "command `{cmd}` argument {} expects {}, got {}",
                        i + 1,
                        t.as_str(),
                        v.ty.as_str()
                    ))
                }
                _ => {}
            }
            arg_vals.push(v);
        }
        let arg_tys: Vec<Ty> = arg_vals.iter().map(|v| v.ty).collect();
        let ret_ty = resolve_ret(&sig, &arg_tys);

        let argc = arg_vals.len();
        // Return slot (always allocated; ignored for void commands). Reserved
        // in the prologue, not here: this code runs wherever the call appears,
        // and an `alloca` in a loop body reserves fresh space on every turn
        // that nothing reclaims until the function returns. A loop calling a
        // command used to exhaust the stack at around a quarter of a million
        // iterations. The size is a constant per call site, so each site gets
        // its own slot and no two calls share one.
        let ret_slot = self.alloca_temp("%Slot");

        // argv array + per-argument stores.
        let argv_base = if argc > 0 {
            let argv = self.alloca_temp(&format!("[{argc} x %Slot]"));
            for (i, v) in arg_vals.iter().enumerate() {
                let raw = self.emit_arg_i64(v);
                let slot = self.fresh();
                writeln!(
                    self.body,
                    "  {slot} = getelementptr [{argc} x %Slot], ptr {argv}, i64 0, i64 {i}"
                )
                .unwrap();
                let tagp = self.fresh();
                writeln!(
                    self.body,
                    "  {tagp} = getelementptr %Slot, ptr {slot}, i32 0, i32 0"
                )
                .unwrap();
                writeln!(self.body, "  store i32 {}, ptr {tagp}", v.ty.sdt_tag()).unwrap();
                let valp = self.fresh();
                writeln!(
                    self.body,
                    "  {valp} = getelementptr %Slot, ptr {slot}, i32 0, i32 2"
                )
                .unwrap();
                writeln!(self.body, "  store i64 {raw}, ptr {valp}").unwrap();
            }
            let base = self.fresh();
            writeln!(
                self.body,
                "  {base} = getelementptr [{argc} x %Slot], ptr {argv}, i64 0, i64 0"
            )
            .unwrap();
            base
        } else {
            "null".to_string()
        };

        self.used.insert(symbol.clone());
        writeln!(
            self.body,
            "  call void @{symbol}(ptr {ret_slot}, i32 {argc}, ptr {argv_base})"
        )
        .unwrap();

        match ret_ty {
            None => Ok(None),
            Some(rt) => {
                let valp = self.fresh();
                writeln!(
                    self.body,
                    "  {valp} = getelementptr %Slot, ptr {ret_slot}, i32 0, i32 2"
                )
                .unwrap();
                let raw = self.fresh();
                writeln!(self.body, "  {raw} = load i64, ptr {valp}").unwrap();
                let res = self.emit_ret_from_i64(rt, &raw);
                Ok(Some(Val {
                    ty: rt,
                    operand: res,
                }))
            }
        }
    }
}
