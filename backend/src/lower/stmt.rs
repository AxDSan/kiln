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
    /// Evaluate a condition and branch. LLVM needs an `i1`, and bools are held
    /// as `i32`, so compare against zero at the branch.
    pub(crate) fn branch_on(&mut self, cond: &Expr, yes: &str, no: &str) -> Result<(), LowerError> {
        let v = self.eval(cond)?;
        if v.ty != Ty::Bool {
            return err(format!(
                "condition must be a truth value, got {}",
                v.ty.as_str()
            ));
        }
        let t = self.fresh();
        writeln!(self.body, "  {t} = icmp ne i32 {}, 0", v.operand).unwrap();
        writeln!(self.body, "  br i1 {t}, label %{yes}, label %{no}").unwrap();
        Ok(())
    }

    pub(crate) fn block(&mut self, stmts: &[kiln_ir::Stmt]) -> Result<(), LowerError> {
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    /// The prologue, with every slot attributed to the subroutine's header.
    ///
    /// The slots are reserved before any statement runs, so there is no
    /// statement to attribute them to — but leaving them bare makes a row in
    /// the line table with no line, covering the addresses a breakpoint on the
    /// function's first line would land on.
    pub(crate) fn prologue(&self, loc: Option<usize>) -> String {
        match loc {
            None => self.allocas.join(""),
            Some(n) => self
                .allocas
                .iter()
                .map(|l| format!("{}, !dbg !{n}\n", l.trim_end_matches('\n')))
                .collect(),
        }
    }

    /// Point the instruction stream at a source position. Every instruction
    /// written after this carries it, until it is changed or cleared.
    /// Describe a named local in the debug information, and bind it to the
    /// stack slot holding it.
    ///
    /// Does nothing when the build carries no debug information, which is what
    /// makes this safe to call from every place a local is bound. A name the
    /// compiler invented is refused by `local` rather than filtered here, so
    /// there is one rule about which names are the user's.
    ///
    /// `flat` is a c-record, whose slot holds the object itself; everything
    /// else holds a value or a pointer to one. Getting that wrong renders a
    /// record's fields out of the eight bytes of a pointer.
    pub(crate) fn describe_local(
        &mut self,
        name: &str,
        slot: &str,
        ty: Ty,
        flat: bool,
        arg: Option<usize>,
    ) {
        let (Some(scope), Some(loc)) = (self.scope, self.body.loc) else {
            return;
        };
        // The line the statement is on, which is where the user declared it.
        let line = self.stmt_line;
        let reg = self.reg;
        let Some(debug) = self.debug.as_mut() else {
            return;
        };
        let ty = match (flat, ty) {
            (true, Ty::Record(record)) => debug.c_storage_type(record, reg),
            (_, ty) => debug.value_type(ty, reg),
        };
        let Some(var) = debug.local(scope, name, ty, line, arg) else {
            return;
        };
        let record = debug.declare(slot, var, loc);
        // Written through the instruction stream like everything else. The
        // stream knows not to give a record a trailing location, and not to
        // give a second one to the intrinsic, which arrives carrying its own.
        write!(self.body, "{record}").unwrap();
    }

    pub(crate) fn set_loc(&mut self, scope: Option<usize>, line: usize, column: usize) {
        self.body.loc = match (scope, self.debug.as_mut()) {
            (Some(sp), Some(d)) => Some(d.location(sp, line, column)),
            _ => None,
        };
    }

    pub(crate) fn store_global(&mut self, name: &str, v: &Val) {
        writeln!(
            self.body,
            "  store {} {}, ptr @{}",
            llvm_ty(v.ty),
            v.operand,
            global_symbol(name)
        )
        .unwrap();
    }

    /// Lower one statement, at its own place in the source.
    ///
    /// The position is set here rather than by the caller, because statements
    /// nest: a `for` lowers the statements of its body through this same
    /// function, and positioning only the outermost ones gave a whole loop
    /// body the loop header's line. There was then no row in the line table
    /// for anything inside a block — a breakpoint could not be put inside a
    /// loop, and stepping through one was impossible.
    ///
    /// A statement whose line was lost keeps the position of whatever encloses
    /// it, rather than reporting line 0, which a debugger reads as "no line
    /// here" and steps straight past.
    ///
    /// The enclosing position is restored afterwards, so a compound
    /// statement's own trailing instructions — a loop's increment, its branch
    /// back — belong to the statement that owns them rather than to the last
    /// statement of its body.
    pub(crate) fn stmt(&mut self, s: &kiln_ir::Stmt) -> Result<(), LowerError> {
        let enclosing = self.body.loc;
        let enclosing_line = self.stmt_line;
        if s.line > 0 {
            self.stmt_line = s.line;
        }
        if self.scope.is_some() && s.line > 0 {
            let scope = self.scope;
            self.set_loc(scope, s.line, s.span.col.max(1));
        }
        let result = self.stmt_at(s);
        self.body.loc = enclosing;
        self.stmt_line = enclosing_line;
        result
    }

    pub(crate) fn stmt_at(&mut self, s: &kiln_ir::Stmt) -> Result<(), LowerError> {
        use kiln_ir::StmtKind;
        match &s.kind {
            // Erased by the desugar; reaching one means the module was lowered
            // without it.
            StmtKind::LetInfer { name, .. } => {
                return err(format!("the type of `{name}` was never worked out"))
            }
            StmtKind::Let {
                name,
                ty,
                value,
                mutable: _,
            } => {
                // A c-record local is a flat struct on the stack, not a pointer
                // to a heap object: allocate its exact size, zero it, and record
                // the name as bound to that storage. There is no value to
                // evaluate — a c-record `var` is only ever the zeroed default —
                // and reading the name later hands back this address, not a
                // load (see the `Var` arm).
                if let Ty::Record(rec) = ty {
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                        let size = self.c_record_size(rec)?;
                        let slot = self.alloca_bytes(size);
                        // The zero-init sits at the statement, not the function
                        // top, so a `var r: RECT` inside a loop re-zeroes each
                        // pass, exactly as C's block-scoped struct would.
                        writeln!(
                            self.body,
                            "  store [{size} x i8] zeroinitializer, ptr {slot}"
                        )
                        .unwrap();
                        self.describe_local(name, &slot, *ty, true, None);
                        self.vars.insert(name.clone(), (slot, *ty));
                        return Ok(());
                    }
                }
                // An optional is two locals, not one: the value in its own
                // width, and the truth beside it saying whether the value is
                // there. Nothing else in lowering knows that — `Unwrap` reads
                // the first, `HasValue` the second, and the checker has already
                // refused every other reading of the name.
                if let Ty::Optional(elem) = ty {
                    let slot = self.alloca(elem.ty());
                    let has = self.alloca(Ty::Bool);
                    self.describe_local(name, &slot, elem.ty(), false, None);
                    self.describe_local(&has_name(name), &has, Ty::Bool, false, None);
                    self.vars.insert(name.clone(), (slot, *ty));
                    self.vars.insert(has_name(name), (has, Ty::Bool));
                    return self.store_optional(name, *elem, value);
                }
                let v = self.eval_hinted(value, Some(*ty))?;
                if v.ty != *ty {
                    return err(format!(
                        "type mismatch in `let {name}`: declared {}, expression is {}",
                        ty.as_str(),
                        v.ty.as_str()
                    ));
                }
                let slot = self.alloca(*ty);
                writeln!(
                    self.body,
                    "  store {} {}, ptr {slot}",
                    llvm_ty(*ty),
                    v.operand
                )
                .unwrap();
                self.describe_local(name, &slot, *ty, false, None);
                self.vars.insert(name.clone(), (slot, *ty));
                Ok(())
            }
            StmtKind::Assign { name, value } => {
                // Assigning to a `var v: T?` rewrites both halves, through the
                // same code the declaration used — so `v = env_get("K")` and
                // `let v: text? = env_get("K")` mean the same thing.
                if let Some((_, Ty::Optional(elem))) = self.vars.get(name).cloned() {
                    return self.store_optional(name, elem, value);
                }
                let want = self
                    .vars
                    .get(name)
                    .map(|(_, t)| *t)
                    .or_else(|| self.globals.get(name).copied());
                let v = self.eval_hinted(value, want)?;
                if let Some((slot, ty)) = self.vars.get(name).cloned() {
                    if v.ty != ty {
                        return err(format!("cannot assign {} to `{name}`", v.ty.as_str()));
                    }
                    writeln!(
                        self.body,
                        "  store {} {}, ptr {slot}",
                        llvm_ty(ty),
                        v.operand
                    )
                    .unwrap();
                    Ok(())
                } else if self.globals.contains_key(name) {
                    self.store_global(name, &v);
                    Ok(())
                } else {
                    err(format!("assignment to undefined variable `{name}`"))
                }
            }
            StmtKind::SetIndex { name, index, value } => {
                let target = self.eval(&Expr::Var(name.clone()))?;
                let i = self.eval(index)?;
                // A position is an int and a key is text; which one this is
                // depends on what is being subscripted, so the check belongs
                // with each arm below rather than ahead of them.
                if !matches!(target.ty, Ty::Dict(_)) && i.ty != Ty::Int {
                    return err(format!(
                        "an index counts with `int` values, got {}",
                        i.ty.as_str()
                    ));
                }
                match target.ty {
                    Ty::Dict(value_ty) => {
                        if i.ty != Ty::Text {
                            return err(format!(
                                "a dictionary is keyed by text, got {}",
                                i.ty.as_str()
                            ));
                        }
                        let v = self.eval_hinted(value, Some(value_ty.ty()))?;
                        if v.ty != value_ty.ty() {
                            return err(format!(
                                "`{name}` holds {} values, cannot store {}",
                                value_ty.as_str(),
                                v.ty.as_str()
                            ));
                        }
                        let raw = self.emit_arg_i64(&v);
                        self.aggr_used.insert("kn_dict_put");
                        writeln!(
                            self.body,
                            "  call void @kn_dict_put(ptr {}, ptr {}, i64 {raw})",
                            target.operand, i.operand
                        )
                        .unwrap();
                        Ok(())
                    }
                    Ty::Bytes => {
                        let v = self.eval(value)?;
                        if v.ty != Ty::Int {
                            return err(format!(
                                "a byte is written as an `int`, got {}",
                                v.ty.as_str()
                            ));
                        }
                        self.aggr_used.insert("kn_bin_set");
                        writeln!(
                            self.body,
                            "  call void @kn_bin_set(ptr {}, i32 {}, i32 {})",
                            target.operand, i.operand, v.operand
                        )
                        .unwrap();
                        Ok(())
                    }
                    Ty::Array(elem) => {
                        let v = self.eval_hinted(value, Some(elem.ty()))?;
                        if v.ty != elem.ty() {
                            return err(format!(
                                "`{name}` holds {} values, cannot store {}",
                                elem.as_str(),
                                v.ty.as_str()
                            ));
                        }
                        let raw = self.emit_arg_i64(&v);
                        self.aggr_used.insert("kn_ary_set");
                        writeln!(
                            self.body,
                            "  call void @kn_ary_set(ptr {}, i32 {}, i64 {raw})",
                            target.operand, i.operand
                        )
                        .unwrap();
                        Ok(())
                    }
                    other => err(format!(
                        "`{name}` is {} — only an array or a byte-set has elements",
                        other.as_str()
                    )),
                }
            }
            StmtKind::Call { cmd, args } => {
                self.eval_call(cmd, args)?; // any return value discarded
                Ok(())
            }
            StmtKind::CallThrough {
                callee,
                args,
                ret,
                conv,
            } => {
                self.eval_call_through(callee, args, *ret, *conv)?; // result discarded
                Ok(())
            }
            StmtKind::If { arms, otherwise } => {
                let done = self.fresh_label("endif");
                for (cond, body) in arms {
                    let then = self.fresh_label("then");
                    let next = self.fresh_label("elif");
                    self.branch_on(cond, &then, &next)?;
                    writeln!(self.body, "{then}:").unwrap();
                    self.block(body)?;
                    writeln!(self.body, "  br label %{done}").unwrap();
                    writeln!(self.body, "{next}:").unwrap();
                }
                if let Some(body) = otherwise {
                    self.block(body)?;
                }
                writeln!(self.body, "  br label %{done}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                Ok(())
            }
            // A `match` is an if/else-if chain that tests one evaluation of
            // the value. The binding is made here rather than in the parser
            // for the reason `for each`'s is: the hidden local's type is the
            // value's type, and nothing before this knows it. Past the store,
            // every arm is the ordinary `x = v` (or-joined for a `when` that
            // lists several) a hand-written chain would have.
            StmtKind::Match {
                scrutinee,
                arms,
                otherwise,
            } => {
                use kiln_ir::Stmt;
                let line = s.line;
                let v = self.eval(scrutinee)?;
                let ty = v.ty;
                let slot = self.alloca(ty);
                writeln!(
                    self.body,
                    "  store {} {}, ptr {slot}",
                    llvm_ty(ty),
                    v.operand
                )
                .unwrap();
                let name = self.fresh_hidden("match");
                self.vars.insert(name.clone(), (slot, ty));
                let subject = Expr::Var(name);
                let mut if_arms: Vec<(Expr, Vec<Stmt>)> = Vec::new();
                for (values, body) in arms {
                    let mut cond: Option<Expr> = None;
                    for val in values {
                        let test =
                            Expr::Cmp(CmpOp::Eq, Box::new(subject.clone()), Box::new(val.clone()));
                        cond = Some(match cond {
                            None => test,
                            Some(c) => Expr::Logical(LogicalOp::Or, Box::new(c), Box::new(test)),
                        });
                    }
                    let Some(cond) = cond else {
                        return err("a `when` must list at least one value".to_string());
                    };
                    if_arms.push((cond, body.clone()));
                }
                self.stmt(&Stmt::new(
                    StmtKind::If {
                        arms: if_arms,
                        otherwise: otherwise.clone(),
                    },
                    line,
                ))?;
                Ok(())
            }
            StmtKind::While { cond, body } => {
                let head = self.fresh_label("while");
                let inner = self.fresh_label("do");
                let done = self.fresh_label("done");
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{head}:").unwrap();
                self.branch_on(cond, &inner, &done)?;
                writeln!(self.body, "{inner}:").unwrap();
                // A `while` re-tests its condition, so `continue` goes to the
                // head; `break` leaves.
                self.loops.push((head.clone(), done.clone()));
                let r = self.block(body);
                self.loops.pop();
                r?;
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                Ok(())
            }
            StmtKind::For {
                var,
                start,
                limit,
                step,
                body,
            } => {
                // Both bounds are read once, into stack slots, before the loop
                // starts: `for i = 1 to n` where the body changes `n` still
                // runs the number of times it said it would.
                let iv = self.alloca(Ty::Int);
                let sv = self.eval(start)?;
                writeln!(self.body, "  store i32 {}, ptr {iv}", sv.operand).unwrap();
                let lv = self.alloca(Ty::Int);
                let lval = self.eval(limit)?;
                writeln!(self.body, "  store i32 {}, ptr {lv}", lval.operand).unwrap();
                self.describe_local(var, &iv, Ty::Int, false, None);
                self.vars.insert(var.clone(), (iv.clone(), Ty::Int));

                let head = self.fresh_label("for");
                let inner = self.fresh_label("fordo");
                let next = self.fresh_label("fornext");
                let done = self.fresh_label("forend");
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{head}:").unwrap();
                let i = self.fresh();
                writeln!(self.body, "  {i} = load i32, ptr {iv}").unwrap();
                let l = self.fresh();
                writeln!(self.body, "  {l} = load i32, ptr {lv}").unwrap();
                let c = self.fresh();
                // The step's sign is a compile-time fact, so which way the
                // loop counts costs nothing at run time.
                let pred = if *step > 0 { "sle" } else { "sge" };
                writeln!(self.body, "  {c} = icmp {pred} i32 {i}, {l}").unwrap();
                writeln!(self.body, "  br i1 {c}, label %{inner}, label %{done}").unwrap();
                writeln!(self.body, "{inner}:").unwrap();
                self.loops.push((next.clone(), done.clone()));
                let r = self.block(body);
                self.loops.pop();
                r?;
                writeln!(self.body, "  br label %{next}").unwrap();
                writeln!(self.body, "{next}:").unwrap();
                let cur = self.fresh();
                writeln!(self.body, "  {cur} = load i32, ptr {iv}").unwrap();
                let inc = self.fresh();
                writeln!(self.body, "  {inc} = add i32 {cur}, {step}").unwrap();
                writeln!(self.body, "  store i32 {inc}, ptr {iv}").unwrap();
                writeln!(self.body, "  br label %{head}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                Ok(())
            }
            // `for each` is sugar: it lowers to the very `for` above, over a
            // hidden `int` counter from 1 to the collection's length, with the
            // element read out by index each turn. Building that `for` here and
            // handing it back to `self.stmt` keeps every loop — counter,
            // once-only bounds, break/continue — lowered in exactly one place.
            StmtKind::ForEach {
                elem,
                value,
                index,
                coll,
                body,
            } => {
                use kiln_ir::Stmt;
                let line = s.line;
                // Read the collection once and pin it to a hidden slot: the loop
                // runs over one snapshot, so a body that grows it cannot make it
                // longer, matching `for`. Its type is what decides the bindings.
                let cv = self.eval(coll)?;
                let cty = cv.ty;
                let (elem_ty, value_ty) =
                    kiln_ir::foreach_elem_types(cty).ok_or_else(|| LowerError {
                        msg: format!("`for each` cannot iterate {}", cty.as_str()),
                    })?;
                let coll_slot = self.alloca(cty);
                writeln!(
                    self.body,
                    "  store {} {}, ptr {coll_slot}",
                    llvm_ty(cty),
                    cv.operand
                )
                .unwrap();
                let coll_name = self.fresh_hidden("coll");
                self.vars.insert(coll_name.clone(), (coll_slot, cty));
                let coll_ref = Expr::Var(coll_name);
                let i_name = self.fresh_hidden("i");
                let i_ref = Expr::Var(i_name.clone());
                let call = |cmd: &str, args: Vec<Expr>| Expr::Call {
                    cmd: cmd.to_string(),
                    args,
                };

                // How to count the collection and how to read one element into
                // `elem` differ by kind; a dictionary also pulls its keys out
                // once, before the loop, and looks each value up by key.
                let mut loop_body: Vec<Stmt> = Vec::new();
                let count_expr = match cty {
                    Ty::Array(_) => {
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: Expr::Index {
                                    base: Box::new(coll_ref.clone()),
                                    index: Box::new(i_ref.clone()),
                                },
                                mutable: false,
                            },
                            line,
                        ));
                        call("count", vec![coll_ref.clone()])
                    }
                    // Each byte is an `int`, 0..255, the way `bytes_at` reads one.
                    Ty::Bytes => {
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: call("bytes_at", vec![coll_ref.clone(), i_ref.clone()]),
                                mutable: false,
                            },
                            line,
                        ));
                        call("bytes_count", vec![coll_ref.clone()])
                    }
                    // Each character is a one-character `text`; `length` and
                    // `substr` both count characters, so the slice never splits
                    // one and the loop runs once per character.
                    Ty::Text => {
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: call(
                                    "substr",
                                    vec![coll_ref.clone(), i_ref.clone(), Expr::IntLit(1)],
                                ),
                                mutable: false,
                            },
                            line,
                        ));
                        call("length", vec![coll_ref.clone()])
                    }
                    Ty::Dict(_) => {
                        let keys_name = self.fresh_hidden("keys");
                        self.stmt(&Stmt::new(
                            StmtKind::Let {
                                name: keys_name.clone(),
                                ty: Ty::Array(Elem::Text),
                                value: call("dict_keys", vec![coll_ref.clone()]),
                                mutable: false,
                            },
                            line,
                        ))?;
                        let keys_ref = Expr::Var(keys_name);
                        // The element binding is the key.
                        loop_body.push(Stmt::new(
                            StmtKind::Let {
                                name: elem.clone(),
                                ty: elem_ty,
                                value: Expr::Index {
                                    base: Box::new(keys_ref.clone()),
                                    index: Box::new(i_ref.clone()),
                                },
                                mutable: false,
                            },
                            line,
                        ));
                        if let Some(v) = value {
                            loop_body.push(Stmt::new(
                                StmtKind::Let {
                                    name: v.clone(),
                                    ty: value_ty.unwrap_or(elem_ty),
                                    value: call(
                                        "dict_get",
                                        vec![coll_ref.clone(), Expr::Var(elem.clone())],
                                    ),
                                    mutable: false,
                                },
                                line,
                            ));
                        }
                        call("count", vec![keys_ref])
                    }
                    other => return err(format!("`for each` cannot iterate {}", other.as_str())),
                };
                // `at IDX` — the 1-based position, which is the counter itself.
                if let Some(idx) = index {
                    loop_body.push(Stmt::new(
                        StmtKind::Let {
                            name: idx.clone(),
                            ty: Ty::Int,
                            value: i_ref,
                            mutable: false,
                        },
                        line,
                    ));
                }
                // The author's body runs after the bindings, each turn.
                loop_body.extend(body.iter().cloned());
                self.stmt(&Stmt::new(
                    StmtKind::For {
                        var: i_name,
                        start: Expr::IntLit(1),
                        limit: count_expr,
                        step: 1,
                        body: loop_body,
                    },
                    line,
                ))?;
                Ok(())
            }
            StmtKind::Break | StmtKind::Continue => {
                let is_break = matches!(s.kind, StmtKind::Break);
                let Some((next, done)) = self.loops.last().cloned() else {
                    return err(format!(
                        "`{}` outside a loop",
                        if is_break { "break" } else { "continue" }
                    ));
                };
                let target = if is_break { done } else { next };
                writeln!(self.body, "  br label %{target}").unwrap();
                // The jump terminates this block; whatever follows it in the
                // source is unreachable but still needs somewhere to live.
                let dead = self.fresh_label("postjump");
                writeln!(self.body, "{dead}:").unwrap();
                Ok(())
            }
            // Copied to the block's exits by `expand_defer` before lowering
            // begins; reaching one means a body was lowered without that pass.
            StmtKind::Defer(_) => err("a `defer` was never copied to the block's exits"),
            // Expanded by the desugar into an `if`; reaching one means the
            // module was lowered without that pass.
            StmtKind::IfSome { bind, .. } => err(format!(
                "`if some ... as {bind}` was never expanded into an `if`"
            )),
            StmtKind::Return { value } => {
                match value {
                    None => writeln!(self.body, "  ret void").unwrap(),
                    Some(e) => {
                        let v = self.eval_hinted(e, self.ret_ty)?;
                        writeln!(self.body, "  ret {} {}", llvm_ty(v.ty), v.operand).unwrap();
                    }
                }
                // `ret` terminates the block. Anything the author wrote after it
                // is unreachable, but LLVM still needs somewhere to put it — so
                // open a fresh block rather than emitting into a closed one.
                let dead = self.fresh_label("postret");
                writeln!(self.body, "{dead}:").unwrap();
                Ok(())
            }
            // `r.pt.x = v`, `r.rgb[3] = v` — a store through a path into a
            // c-record's flat storage. The address comes from the same walker a
            // read uses, so a write can never land at a different offset than
            // the read of the same words would.
            StmtKind::SetPlace { place, value } => {
                let (p, fty) = self.c_place_ptr(place)?;
                self.c_store(&p, fty, value, "that field")
            }
            StmtKind::SetProperty {
                component,
                property,
                value,
            } => {
                if let Some(Ty::Record(rec)) = self.var_ty(component) {
                    // A c-record's field is a store into flat storage; the plain
                    // record's stays the heap `kn_rec_set` below.
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                        let base = self.eval(&Expr::Var(component.clone()))?;
                        return self.emit_c_field_write(rec, &base.operand, property, value);
                    }
                    let def = self.reg.record(rec).cloned().ok_or_else(|| LowerError {
                        msg: format!("unknown record `{rec}`"),
                    })?;
                    let (pos, want) = def.field(property).ok_or_else(|| LowerError {
                        msg: format!("record `{rec}` has no field `{property}`"),
                    })?;
                    let base = self.eval(&Expr::Var(component.clone()))?;
                    let v = self.eval_hinted(value, Some(want))?;
                    if v.ty != want {
                        return err(format!(
                            "`{component}.{property}` is {}, cannot store {}",
                            want.as_str(),
                            v.ty.as_str()
                        ));
                    }
                    let raw = self.emit_arg_i64(&v);
                    self.aggr_used.insert("kn_rec_set");
                    writeln!(
                        self.body,
                        "  call void @kn_rec_set(ptr {}, i32 {pos}, i64 {raw})",
                        base.operand
                    )
                    .unwrap();
                    return Ok(());
                }
                let handle = self.handle_of(component)?;
                let v = self.eval(value)?;
                // The D10 boundary takes textual values, so convert first.
                let text = self.value_as_text(&v)?;
                let n = self.cstr(property);
                let f = self.setter(self.owner(component).as_deref());
                writeln!(
                    self.body,
                    "  call i32 @{f}(i64 {handle}, ptr {n}, ptr {text})"
                )
                .unwrap();
                Ok(())
            }
        }
    }

    /// The compile-time handle constant for a component id.
    pub(crate) fn handle_of(&self, id: &str) -> Result<u64, LowerError> {
        self.handles.get(id).copied().ok_or_else(|| LowerError {
            msg: format!("unknown component `{id}`"),
        })
    }

    /// Render a value as a `ptr` to text, converting numbers via the runtime.
    pub(crate) fn value_as_text(&mut self, v: &Val) -> Result<String, LowerError> {
        match v.ty {
            Ty::Text => Ok(v.operand.clone()),
            // A property is textual at the D10 boundary, and `true`/`false` is
            // what both a descriptor's default value and the property parser on
            // the other side already spell — so `t.enabled = false` reaches the
            // component as the same words the source wrote.
            Ty::Bool => {
                let yes = self.cstr("true");
                let no = self.cstr("false");
                let c = self.fresh();
                writeln!(self.body, "  {c} = icmp ne i32 {}, 0", v.operand).unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = select i1 {c}, ptr {yes}, ptr {no}").unwrap();
                Ok(t)
            }
            other if !other.is_numeric() => err(format!(
                "cannot use {} where text is expected",
                other.as_str()
            )),
            _ => {
                let sym = match v.ty {
                    Ty::Int => "kn_int_to_text",
                    Ty::Int64 => "kn_int64_to_text",
                    _ => "kn_double_to_text",
                };
                let converted = self.call_symbol_1(sym, v)?;
                Ok(converted)
            }
        }
    }

    /// Compare two text values by content via the runtime, yielding i32 0/1.
    pub(crate) fn call_text_eq(&mut self, a: &Val, b: &Val) -> Result<String, LowerError> {
        self.call_symbol_2("kn_text_eq", a, b, Ty::Bool)
    }

    /// Abort with a runtime error message. `kn_notify(KN_NRS_RUNTIME_ERR, ...)`
    /// prints and exits, so the block ends `unreachable`.
    pub(crate) fn runtime_error(&mut self, message: &str) {
        let m = self.cstr(message);
        self.needs_notify = true;
        writeln!(self.body, "  call ptr @kn_notify(i32 5, ptr {m}, ptr null)").unwrap();
        writeln!(self.body, "  unreachable").unwrap();
    }

    /// Emit the checks integer `/` and `%` need before the hardware sees them:
    /// a zero divisor, and the one overflowing case (the most negative value
    /// divided by -1). A literal divisor is checked here at compile time, so
    /// `x / 2` still lowers to a bare `sdiv`.
    pub(crate) fn guard_divisor(
        &mut self,
        op: BinOp,
        lv: &Val,
        rv: &Val,
    ) -> Result<(), LowerError> {
        let what = if op == BinOp::Div {
            "division"
        } else {
            "remainder"
        };
        let ity = llvm_ty(lv.ty);
        let literal = rv.operand.parse::<i64>().ok();
        if literal == Some(0) {
            return err(format!("{what} by zero"));
        }
        if literal.is_none() {
            let bad = self.fresh();
            writeln!(self.body, "  {bad} = icmp eq {ity} {}, 0", rv.operand).unwrap();
            let trap = self.fresh_label("divzero");
            let ok = self.fresh_label("divok");
            writeln!(self.body, "  br i1 {bad}, label %{trap}, label %{ok}").unwrap();
            writeln!(self.body, "{trap}:").unwrap();
            self.runtime_error(&format!("{what} by zero"));
            writeln!(self.body, "{ok}:").unwrap();
        }
        // `MIN / -1` has no representable answer and faults just as hard as a
        // zero divisor. Only reachable when the divisor can be -1.
        if literal.is_none() || literal == Some(-1) {
            let min = if lv.ty == Ty::Int64 {
                i64::MIN.to_string()
            } else {
                i32::MIN.to_string()
            };
            let is_min = self.fresh();
            writeln!(
                self.body,
                "  {is_min} = icmp eq {ity} {}, {min}",
                lv.operand
            )
            .unwrap();
            let bad = if literal == Some(-1) {
                is_min
            } else {
                let neg1 = self.fresh();
                writeln!(self.body, "  {neg1} = icmp eq {ity} {}, -1", rv.operand).unwrap();
                let both = self.fresh();
                writeln!(self.body, "  {both} = and i1 {is_min}, {neg1}").unwrap();
                both
            };
            let trap = self.fresh_label("divover");
            let ok = self.fresh_label("divok");
            writeln!(self.body, "  br i1 {bad}, label %{trap}, label %{ok}").unwrap();
            writeln!(self.body, "{trap}:").unwrap();
            self.runtime_error(&format!(
                "{what} overflowed: the most negative {} divided by -1",
                lv.ty.as_str()
            ));
            writeln!(self.body, "{ok}:").unwrap();
        }
        Ok(())
    }
}
