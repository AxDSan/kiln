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
    /// `a band b`, `x shl 8`. The checker has already agreed the types; this
    /// repeats enough of the rule to pick the right LLVM instruction and to
    /// refuse rather than emit nonsense if it is ever reached without one.
    pub(crate) fn eval_bit(
        &mut self,
        op: BitOp,
        l: &Expr,
        r: &Expr,
        hint: Option<Ty>,
    ) -> Result<Val, LowerError> {
        let want = match hint {
            Some(Ty::Int) | Some(Ty::Int64) => hint,
            _ => None,
        };
        let lv = self.eval_hinted(l, want)?;
        if !matches!(lv.ty, Ty::Int | Ty::Int64) {
            return err(format!(
                "`{}` works on int and int64 values; its left side is {}",
                op.word(),
                lv.ty.as_str()
            ));
        }
        if op.is_shift() {
            return self.eval_shift(op, lv, r);
        }
        // The narrow side of a mixed pair is widened by re-reading it as a
        // 64-bit pattern. Only a literal (or a constant, which is one) changes
        // type under a hint — the checker proved that — so the value already
        // emitted for it is a bare constant and emitting it again costs no
        // instruction.
        let rv = self.eval_hinted(
            r,
            if lv.ty == Ty::Int64 {
                Some(Ty::Int64)
            } else {
                want
            },
        )?;
        let (lv, rv) = if lv.ty == rv.ty {
            (lv, rv)
        } else if rv.ty == Ty::Int64 {
            (self.eval_hinted(l, Some(Ty::Int64))?, rv)
        } else {
            return err(format!(
                "`{}` needs both sides to be the same width: {} vs {}",
                op.word(),
                lv.ty.as_str(),
                rv.ty.as_str()
            ));
        };
        let opcode = match op {
            BitOp::And => "and",
            BitOp::Or => "or",
            BitOp::Xor => "xor",
            _ => unreachable!("shifts left above"),
        };
        let t = self.fresh();
        writeln!(
            self.body,
            "  {t} = {opcode} {} {}, {}",
            llvm_ty(lv.ty),
            lv.operand,
            rv.operand
        )
        .unwrap();
        Ok(Val {
            ty: lv.ty,
            operand: t,
        })
    }

    /// `x shl n`, `x shr n`, `x ushr n`.
    ///
    /// The count is a count, not a second value: it is brought to the value's
    /// own width and the result is the value's type. A count at or beyond that
    /// width has no answer the hardware agrees on — LLVM calls the result
    /// poison, which is a silent wrong answer later — so a count written as a
    /// literal is refused at build time, and one computed at run time is taken
    /// modulo the width. That is one `and` instruction, no branch, and what
    /// the machine would have done anyway.
    pub(crate) fn eval_shift(&mut self, op: BitOp, lv: Val, r: &Expr) -> Result<Val, LowerError> {
        let rv = self.eval(r)?;
        if !matches!(rv.ty, Ty::Int | Ty::Int64) {
            return err(format!(
                "the count `{}` shifts by must be int or int64, not {}",
                op.word(),
                rv.ty.as_str()
            ));
        }
        let width: i64 = if lv.ty == Ty::Int64 { 64 } else { 32 };
        let count = if let Ok(k) = rv.operand.parse::<i64>() {
            if !(0..width).contains(&k) {
                return err(format!(
                    "`{}` by {k}: an {} can be shifted by 0 to {}",
                    op.word(),
                    lv.ty.as_str(),
                    width - 1
                ));
            }
            k.to_string()
        } else {
            let c = if rv.ty == lv.ty {
                rv.operand.clone()
            } else {
                let t = self.fresh();
                if lv.ty == Ty::Int64 {
                    writeln!(self.body, "  {t} = sext i32 {} to i64", rv.operand).unwrap();
                } else {
                    writeln!(self.body, "  {t} = trunc i64 {} to i32", rv.operand).unwrap();
                }
                t
            };
            let m = self.fresh();
            writeln!(
                self.body,
                "  {m} = and {} {c}, {}",
                llvm_ty(lv.ty),
                width - 1
            )
            .unwrap();
            m
        };
        let opcode = match op {
            BitOp::Shl => "shl",
            BitOp::Shr => "ashr",
            BitOp::Ushr => "lshr",
            _ => unreachable!("only shifts reach here"),
        };
        let t = self.fresh();
        writeln!(
            self.body,
            "  {t} = {opcode} {} {}, {count}",
            llvm_ty(lv.ty),
            lv.operand
        )
        .unwrap();
        Ok(Val {
            ty: lv.ty,
            operand: t,
        })
    }

    /// `bnot x` — every bit flipped, which is `xor` with all ones.
    pub(crate) fn eval_bitnot(&mut self, e: &Expr, hint: Option<Ty>) -> Result<Val, LowerError> {
        let want = match hint {
            Some(Ty::Int) | Some(Ty::Int64) => hint,
            _ => None,
        };
        let v = self.eval_hinted(e, want)?;
        if !matches!(v.ty, Ty::Int | Ty::Int64) {
            return err(format!(
                "`bnot` flips the bits of an int or an int64, got {}",
                v.ty.as_str()
            ));
        }
        let t = self.fresh();
        writeln!(self.body, "  {t} = xor {} {}, -1", llvm_ty(v.ty), v.operand).unwrap();
        Ok(Val {
            ty: v.ty,
            operand: t,
        })
    }

    /// Evaluate `e` once, store it in a fresh synthetic local, and return that
    /// local's name. The value can then be read back through `Expr::Var(name)`
    /// as many times as the desugar needs while running `e` only once. The name
    /// begins with `$`, which no source identifier can contain, so a temp never
    /// shadows a program's own variable.
    /// The local an optional's two halves are kept under. Only a name reaches
    /// here: `HasValue` and `Unwrap` are made by the desugar out of a binding,
    /// never out of a computed value, because the two halves have to be read
    /// from the same storage the one test wrote.
    pub(crate) fn optional_name(&self, e: &Expr) -> Result<String, LowerError> {
        match e {
            Expr::Var(n) if matches!(self.vars.get(n), Some((_, Ty::Optional(_)))) => Ok(n.clone()),
            _ => err("a value that may be absent is read through the name it was bound to"),
        }
    }

    /// Write both halves of the optional local `name`: the value, and the truth
    /// beside it.
    ///
    /// What sets the truth depends on what the initializer is, and there are
    /// only three kinds:
    ///
    ///  * `none` — nothing is there, and the value half is the zero of its type
    ///    so that a stray read is a zero rather than whatever was on the stack;
    ///  * a **call** — the value is there when the call did not fail. The error
    ///    slot is cleared first, so what is read afterwards is *this* call's
    ///    verdict and not one an earlier failure left behind — a command that
    ///    cannot fail never touches the slot, and without the clear it would
    ///    inherit the last failure in the program;
    ///  * anything else — a value written down is there.
    ///
    /// Copying one optional into another copies both halves, since the source
    /// already carries its own answer.
    pub(crate) fn store_optional(
        &mut self,
        name: &str,
        elem: Elem,
        value: &Expr,
    ) -> Result<(), LowerError> {
        let (slot, _) = self.vars[name].clone();
        let (has_slot, _) = self.vars[&has_name(name)].clone();
        let store = |lo: &mut Self, operand: &str| {
            writeln!(
                lo.body,
                "  store {} {operand}, ptr {slot}",
                llvm_ty(elem.ty())
            )
            .unwrap();
        };
        let set_has = |lo: &mut Self, operand: &str| {
            writeln!(lo.body, "  store i32 {operand}, ptr {has_slot}").unwrap();
        };
        match value {
            Expr::NoneLit => {
                let zero = zero_operand(elem.ty());
                store(self, &zero);
                set_has(self, "0");
            }
            // One optional into another: both halves travel together, because
            // the answer to "is it there" belongs to the value and not to the
            // moment of copying.
            Expr::Var(n) if matches!(self.vars.get(n), Some((_, Ty::Optional(_)))) => {
                let v = self.eval(&Expr::Unwrap(Box::new(value.clone())))?;
                store(self, &v.operand);
                let h = self.eval(&Expr::HasValue(Box::new(value.clone())))?;
                set_has(self, &h.operand);
            }
            Expr::Call { .. } | Expr::CallThrough { .. } => {
                self.needs_error_clear = true;
                writeln!(self.body, "  call void @kn_error_clear()").unwrap();
                let v = self.eval_hinted(value, Some(elem.ty()))?;
                if v.ty != elem.ty() {
                    return err(format!(
                        "`{name}` holds {}, and the call yields {}",
                        elem.as_str(),
                        v.ty.as_str()
                    ));
                }
                store(self, &v.operand);
                let code = self.eval(&Expr::Call {
                    cmd: "last_error_code".to_string(),
                    args: Vec::new(),
                })?;
                let t = self.fresh();
                writeln!(self.body, "  {t} = icmp eq i32 {}, 0", code.operand).unwrap();
                let w = self.fresh();
                writeln!(self.body, "  {w} = zext i1 {t} to i32").unwrap();
                set_has(self, &w);
            }
            _ => {
                let v = self.eval_hinted(value, Some(elem.ty()))?;
                if v.ty != elem.ty() {
                    return err(format!(
                        "`{name}` holds {}, and the value is {}",
                        elem.as_str(),
                        v.ty.as_str()
                    ));
                }
                store(self, &v.operand);
                set_has(self, "1");
            }
        }
        Ok(())
    }

    /// `[EXPR for each x in xs where COND]` — build the array a loop would have
    /// built, and hand it back as the value.
    ///
    /// The three statements written here are the three a program writes by hand:
    /// an empty list, a `for each` over the same collection, and an `append`
    /// inside it — under an `if` when a `where` was written. They go through
    /// `self.stmt`, so the loop is the *same* loop, break/continue and all, and
    /// the append is the same `append` (which copies, so the hidden name is
    /// reassigned each turn, exactly as a hand-written accumulator would be).
    pub(crate) fn eval_comprehension(&mut self, e: &Expr) -> Result<Val, LowerError> {
        use kiln_ir::{Stmt, StmtKind};
        let Expr::Comprehension {
            body,
            elem,
            value,
            index,
            coll,
            cond,
            holds,
        } = e
        else {
            return err("not a list comprehension");
        };
        let Some(holds) = holds else {
            return err("a list built by a loop was never told what it holds");
        };
        let acc = self.fresh_hidden("list");
        let line = 0;
        self.stmt(&Stmt::new(
            StmtKind::Let {
                name: acc.clone(),
                ty: Ty::Array(*holds),
                value: Expr::ArrayLit(Vec::new()),
                mutable: true,
            },
            line,
        ))?;
        let push = Stmt::new(
            StmtKind::Assign {
                name: acc.clone(),
                value: Expr::Call {
                    cmd: "append".to_string(),
                    args: vec![Expr::Var(acc.clone()), (**body).clone()],
                },
            },
            line,
        );
        let inner = match cond {
            None => vec![push],
            Some(c) => vec![Stmt::new(
                StmtKind::If {
                    arms: vec![((**c).clone(), vec![push])],
                    otherwise: None,
                },
                line,
            )],
        };
        self.stmt(&Stmt::new(
            StmtKind::ForEach {
                elem: elem.clone(),
                value: value.clone(),
                index: index.clone(),
                coll: (**coll).clone(),
                body: inner,
            },
            line,
        ))?;
        self.eval(&Expr::Var(acc))
    }

    pub(crate) fn bind_temp(&mut self, e: &Expr) -> Result<String, LowerError> {
        let v = self.eval(e)?;
        let slot = self.alloca(v.ty);
        writeln!(
            self.body,
            "  store {} {}, ptr {slot}",
            llvm_ty(v.ty),
            v.operand
        )
        .unwrap();
        let name = format!("$t{}", self.tmp);
        self.tmp += 1;
        self.vars.insert(name.clone(), (slot, v.ty));
        Ok(name)
    }

    /// `lo <op1> mid <op2> hi` — bind `lo` then `mid` to temps, in that order,
    /// so evaluation runs left to right and the middle runs exactly once, then
    /// lower the plain conjunction `lo <op1> mid and mid <op2> hi`. Reusing
    /// `Cmp` and `Logical` gets text comparison and the `and`'s short circuit
    /// (which keeps `hi` lazy) for free.
    pub(crate) fn eval_chain(
        &mut self,
        lo: &Expr,
        lo_op: CmpOp,
        mid: &Expr,
        hi_op: CmpOp,
        hi: &Expr,
    ) -> Result<Val, LowerError> {
        let lo_t = self.bind_temp(lo)?;
        let mid_t = self.bind_temp(mid)?;
        let desugared = Expr::Logical(
            LogicalOp::And,
            Box::new(Expr::Cmp(
                lo_op,
                Box::new(Expr::Var(lo_t)),
                Box::new(Expr::Var(mid_t.clone())),
            )),
            Box::new(Expr::Cmp(
                hi_op,
                Box::new(Expr::Var(mid_t)),
                Box::new(hi.clone()),
            )),
        );
        self.eval(&desugared)
    }

    /// `xs[a..b]` — bind the base and both bounds to temps, in the order they
    /// were written, then lower to the command the base's type answers to:
    /// `substr` for text, `bytes_slice` for a byte-set, `slice` for an array.
    ///
    /// Temps rather than the expressions themselves because each appears twice
    /// in the rewrite — the base in the length, `from` in the count — and
    /// `s[f()..g()]` must call each of them once. A missing bound is filled in
    /// from the temp: `from` absent is 1, `to` absent is the base's own length,
    /// which is a read of a value already computed and not a second evaluation.
    ///
    /// The commands take a **count**, and the slice is inclusive at both ends,
    /// so the count is `to - from + 1`. Clamping is the command's: a bound
    /// outside the collection is trimmed, never an error.
    pub(crate) fn eval_slice(
        &mut self,
        base: &Expr,
        from: Option<&Expr>,
        to: Option<&Expr>,
    ) -> Result<Val, LowerError> {
        let base_t = self.bind_temp(base)?;
        let bty = self.var_ty(&base_t).expect("temp just bound");
        let (cmd, len_cmd) = match bty {
            Ty::Text => ("substr", "length"),
            Ty::Bytes => ("bytes_slice", "bytes_count"),
            Ty::Array(_) => ("slice", "count"),
            other => {
                return err(format!(
                    "{} cannot be sliced — `a..b` takes a run of text, of bytes, or of a list",
                    other.as_str()
                ))
            }
        };
        // The three commands clamp a start below 1 up to 1 but leave the count
        // alone, so a raw `substr(s, 0, 4)` reads four characters from the
        // first. `a..b` counts POSITIONS, and position 0 is not one of them, so
        // the start is raised to 1 here — before the count is measured from it —
        // and `s[0..3]` is the three characters at 1, 2 and 3.
        let start = match from {
            // A literal start already at or past 1 needs no guard, which is
            // almost every slice ever written.
            Some(Expr::IntLit(v)) if *v >= 1 => Expr::IntLit(*v),
            Some(e) => Expr::Call {
                cmd: "max_int".to_string(),
                args: vec![Expr::Var(self.bind_temp(e)?), Expr::IntLit(1)],
            },
            None => Expr::IntLit(1),
        };
        let end = match to {
            Some(e) => Expr::Var(self.bind_temp(e)?),
            None => Expr::Call {
                cmd: len_cmd.to_string(),
                args: vec![Expr::Var(base_t.clone())],
            },
        };
        // `to - from + 1`: both ends are included, so a one-position slice
        // `s[3..3]` is a count of one. `start` may be a `max_int` call, so it is
        // bound to a temp when it is one — it appears twice below.
        let start = match start {
            Expr::IntLit(v) => Expr::IntLit(v),
            other => Expr::Var(self.bind_temp(&other)?),
        };
        let count = Expr::Bin(
            BinOp::Add,
            Box::new(Expr::Bin(
                BinOp::Sub,
                Box::new(end),
                Box::new(start.clone()),
            )),
            Box::new(Expr::IntLit(1)),
        );
        self.eval(&Expr::Call {
            cmd: cmd.to_string(),
            args: vec![Expr::Var(base_t), start, count],
        })
    }

    /// `e in xs` / `k in d` / `sub in text` — bind the haystack once (its type
    /// picks the command and its value feeds it, and it may have side effects),
    /// then lower to the command that answers membership: `index_of(xs, e) <> 0`
    /// for an array, `dict_has(d, k)` for a dictionary, `find(text, sub) <> 0`
    /// for a substring. `not in` wraps the result in `not`. The needle appears
    /// once in the desugar, so it stays inline and runs once too.
    pub(crate) fn eval_in(
        &mut self,
        needle: &Expr,
        haystack: &Expr,
        negated: bool,
    ) -> Result<Val, LowerError> {
        let hay = self.bind_temp(haystack)?;
        let hty = self.var_ty(&hay).expect("temp just bound");
        let hvar = Expr::Var(hay);
        let desugared = match hty {
            Ty::Array(_) => Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::Call {
                    cmd: "index_of".to_string(),
                    args: vec![hvar, needle.clone()],
                }),
                Box::new(Expr::IntLit(0)),
            ),
            Ty::Dict(_) => Expr::Call {
                cmd: "dict_has".to_string(),
                args: vec![hvar, needle.clone()],
            },
            Ty::Text => Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::Call {
                    cmd: "find".to_string(),
                    args: vec![hvar, needle.clone()],
                }),
                Box::new(Expr::IntLit(0)),
            ),
            other => return err(format!("`in` cannot test membership in {}", other.as_str())),
        };
        let desugared = if negated {
            Expr::Not(Box::new(desugared))
        } else {
            desugared
        };
        self.eval(&desugared)
    }

    /// `if COND then A else B` as a value — one slot, two branches, exactly the
    /// shape `and`/`or` already lower to.
    ///
    /// The `then` arm is evaluated first so its type is known before the slot
    /// is reserved; allocas are emitted at the top of the function, so
    /// reserving one part-way through the body is only a bookkeeping order.
    /// Only one arm runs, so a call in the arm not taken never happens.
    pub(crate) fn eval_ifelse(
        &mut self,
        cond: &Expr,
        then: &Expr,
        els: &Expr,
        hint: Option<Ty>,
    ) -> Result<Val, LowerError> {
        let then_l = self.fresh_label("then_v");
        let else_l = self.fresh_label("else_v");
        let done = self.fresh_label("ifval");
        self.branch_on(cond, &then_l, &else_l)?;

        writeln!(self.body, "{then_l}:").unwrap();
        let tv = self.eval_hinted(then, hint)?;
        let slot = self.alloca(tv.ty);
        writeln!(
            self.body,
            "  store {} {}, ptr {slot}",
            llvm_ty(tv.ty),
            tv.operand
        )
        .unwrap();
        writeln!(self.body, "  br label %{done}").unwrap();

        writeln!(self.body, "{else_l}:").unwrap();
        let ev = self.eval_hinted(els, hint.or(Some(tv.ty)))?;
        if ev.ty != tv.ty {
            return err(format!(
                "both sides of `if` must have one type; `then` is {} and `else` is {}",
                tv.ty.as_str(),
                ev.ty.as_str()
            ));
        }
        writeln!(
            self.body,
            "  store {} {}, ptr {slot}",
            llvm_ty(ev.ty),
            ev.operand
        )
        .unwrap();
        writeln!(self.body, "  br label %{done}").unwrap();

        writeln!(self.body, "{done}:").unwrap();
        let t = self.fresh();
        writeln!(self.body, "  {t} = load {}, ptr {slot}", llvm_ty(tv.ty)).unwrap();
        Ok(Val {
            ty: tv.ty,
            operand: t,
        })
    }

    /// `EXPR otherwise FALLBACK` — run `EXPR` into a temporary, then lower
    /// *literally* the desugar the language documents:
    /// `if last_error_code() <> 0 then FALLBACK else <that temporary>`.
    ///
    /// Reusing the conditional means there is one branch semantics in the
    /// backend, not two, and the fallback stays lazy for free: it is an arm, so
    /// it runs only when the call failed.
    pub(crate) fn eval_otherwise(
        &mut self,
        value: &Expr,
        fallback: &Expr,
        hint: Option<Ty>,
    ) -> Result<Val, LowerError> {
        // A value that may be absent carries its own answer: `otherwise` reads
        // the truth beside it rather than the error slot, so a fallback taken
        // long after the call that failed is still taken for the right reason.
        if let Expr::Var(n) = value {
            if let Some((_, Ty::Optional(elem))) = self.vars.get(n).cloned() {
                let desugared = Expr::IfElse {
                    cond: Box::new(Expr::HasValue(Box::new(value.clone()))),
                    then: Box::new(Expr::Unwrap(Box::new(value.clone()))),
                    els: Box::new(fallback.clone()),
                };
                return self.eval_hinted(&desugared, hint.or(Some(elem.ty())));
            }
        }
        let t = self.bind_temp(value)?;
        // The fallback becomes the `then` arm, so it is the one an untyped
        // literal sits in: `f() otherwise []` in a position that declares
        // nothing has only the value's own type to take. The checker agreed to
        // exactly this fallback (`hint.or(Some(value_ty))`).
        let vty = self.var_ty(&t);
        let desugared = Expr::IfElse {
            cond: Box::new(Expr::Cmp(
                CmpOp::Ne,
                Box::new(Expr::Call {
                    cmd: "last_error_code".to_string(),
                    args: Vec::new(),
                }),
                Box::new(Expr::IntLit(0)),
            )),
            then: Box::new(fallback.clone()),
            els: Box::new(Expr::Var(t)),
        };
        self.eval_hinted(&desugared, hint.or(vty))
    }
}
