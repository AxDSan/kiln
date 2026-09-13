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
    pub(crate) fn fresh(&mut self) -> String {
        let t = format!("%t{}", self.tmp);
        self.tmp += 1;
        t
    }

    /// A hidden local variable name for a desugaring — the counter and the
    /// collection snapshot a `for each` lowers through. It carries a `$`, which
    /// no source identifier can, so it never shadows a user variable that lands
    /// in the same `self.vars` table.
    pub(crate) fn fresh_hidden(&mut self, tag: &str) -> String {
        let s = format!("$each${tag}${}", self.tmp);
        self.tmp += 1;
        s
    }

    /// Emit a string constant and return an operand pointing at it.
    pub(crate) fn cstr(&mut self, text: &str) -> String {
        let id = self.strings.len();
        self.strings.push(text.to_string());
        let bytes = text.len() + 1;
        format!("getelementptr inbounds ([{bytes} x i8], ptr @.str{id}, i64 0, i64 0)")
    }

    pub(crate) fn fresh_label(&mut self, kind: &str) -> String {
        let l = format!("bb_{kind}_{}", self.label);
        self.label += 1;
        l
    }

    pub(crate) fn eval(&mut self, e: &Expr) -> Result<Val, LowerError> {
        self.eval_hinted(e, None)
    }

    /// As `eval`, told the type the destination declares.
    ///
    /// Only an empty `[]` needs it — there is no element to take a type from,
    /// so the destination's declaration is the only thing that knows. The
    /// validator has already agreed the hint fits.
    pub(crate) fn eval_hinted(&mut self, e: &Expr, hint: Option<Ty>) -> Result<Val, LowerError> {
        // A bare name that is a constant (and is not a local or a module
        // variable) folds to its literal here, before any other rule — so the
        // constant is lowered exactly as the literal it stands for, including
        // the `int64` widening below. The checker agreed to the same fold.
        if let Expr::Var(name) = e {
            if !self.vars.contains_key(name) && !self.globals.contains_key(name) {
                if let Some(c) = self.reg.const_(name) {
                    let value = c.value.clone();
                    return self.eval_hinted(&value, hint);
                }
            }
        }
        if let Expr::ArrayLit(items) = e {
            let elem = match (items.first(), hint) {
                (Some(first), _) => {
                    let v = self.eval(first)?;
                    Elem::from_ty(v.ty).ok_or_else(|| LowerError {
                        msg: format!("a list cannot hold {} values", v.ty.as_str()),
                    })?
                }
                (None, Some(Ty::Array(elem))) => elem,
                (None, _) => {
                    return err("`[]` here does not say what it holds");
                }
            };
            return self.eval_array_lit(elem, items);
        }
        if let Expr::DictLit(pairs) = e {
            let value = match (pairs.first(), hint) {
                (Some((_, first)), _) => {
                    let v = self.eval(first)?;
                    Elem::from_ty(v.ty).ok_or_else(|| LowerError {
                        msg: format!("a dictionary cannot hold {} values", v.ty.as_str()),
                    })?
                }
                (None, Some(Ty::Dict(value))) => value,
                (None, _) => return err("`{}` here does not say what it holds"),
            };
            return self.eval_dict_lit(value, pairs);
        }
        // The literal-to-`int64` widening the checker already agreed to (see
        // `type_of_expr_hinted`): emit the constant as an i64 so an `int64`
        // parameter or `let` receives it with no `int_to_int64` at the source.
        if let Expr::IntLit(v) = e {
            if hint == Some(Ty::Int64) {
                return Ok(Val {
                    ty: Ty::Int64,
                    operand: v.to_string(),
                });
            }
        }
        // A hex/binary pattern takes the width its destination declares, and
        // gains zeros rather than a sign doing it — `0x8000_0000` is the `int`
        // sign bit on its own and `int64` 2147483648 here. The checker agreed
        // to the same reading (`bits_value_int64`).
        if let Expr::BitsLit(v) = e {
            if hint == Some(Ty::Int64) {
                return Ok(Val {
                    ty: Ty::Int64,
                    operand: kiln_ir::sema::bits_value_int64(*v).to_string(),
                });
            }
        }
        // Bitwise operators are intercepted here, not in `eval_inner`, because
        // their operands want the surrounding hint: `var s: int64 = A bor B`
        // must read both patterns as 64-bit ones, exactly as the checker did.
        if let Expr::Bit(op, l, r) = e {
            return self.eval_bit(*op, l, r, hint);
        }
        if let Expr::BitNot(inner) = e {
            return self.eval_bitnot(inner, hint);
        }
        // Both conditional forms pass the hint to their arms, for the same
        // reason a bitwise operator does: `let n: int64 = if c then 0 else 1`
        // must emit i64 constants in both, and only the destination knows.
        if let Expr::IfElse { cond, then, els } = e {
            return self.eval_ifelse(cond, then, els, hint);
        }
        if let Expr::Otherwise { value, fallback } = e {
            return self.eval_otherwise(value, fallback, hint);
        }
        self.eval_inner(e)
    }

    pub(crate) fn eval_inner(&mut self, e: &Expr) -> Result<Val, LowerError> {
        match e {
            // Intercepted by `eval_hinted`, which is the only caller.
            Expr::Bit(..) | Expr::BitNot(_) => {
                err("a bitwise operator is lowered by `eval_hinted`")
            }
            Expr::IfElse { .. } | Expr::Otherwise { .. } => {
                err("a conditional value is lowered by `eval_hinted`")
            }
            Expr::Comprehension { .. } => self.eval_comprehension(e),
            // `none` never reaches here: it is only ever the initializer of an
            // optional, and `store_optional` reads it before evaluating.
            Expr::NoneLit => err("`none` here does not say what it is the absence of"),
            Expr::HasValue(x) => {
                let name = self.optional_name(x)?;
                let (slot, _) = self.vars[&has_name(&name)].clone();
                let t = self.fresh();
                writeln!(self.body, "  {t} = load i32, ptr {slot}").unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            Expr::Unwrap(x) => {
                let name = self.optional_name(x)?;
                let (slot, ty) = self.vars[&name].clone();
                let Ty::Optional(elem) = ty else {
                    return err(format!("`{name}` holds no value to unwrap"));
                };
                let t = self.fresh();
                writeln!(self.body, "  {t} = load {}, ptr {slot}", llvm_ty(elem.ty())).unwrap();
                Ok(Val {
                    ty: elem.ty(),
                    operand: t,
                })
            }
            Expr::ArrayLit(_) => err("`[]` here does not say what it holds"),
            Expr::DictLit(_) => err("`{}` here does not say what it holds"),
            Expr::RecordLit { name, fields } => self.eval_record_lit(name, fields),
            // Erased by the desugar; reaching one means the module was lowered
            // without it.
            Expr::Labeled { name, .. } => err(format!(
                "the named argument `{name}:` was never matched to a parameter"
            )),
            Expr::RecordUpdate { name, .. } => err(format!(
                "`{name}{{...}}` was never expanded into its fields"
            )),
            Expr::Field { base, name } => {
                let b = self.eval(base)?;
                match b.ty {
                    Ty::Record(rec) => self.emit_field_read(rec, &b, name),
                    other => err(format!(
                        "`.{name}` reads a field, and {} has none",
                        other.as_str()
                    )),
                }
            }
            // One element of a c-record's inline array is an address inside the
            // struct, not a runtime array lookup — route it through the one
            // place walker so a read, a write and `address of` all compute the
            // same offset.
            Expr::Index { base, index } if self.rooted_in_c_record(base) => {
                let (p, ty) = self.c_place_ptr(&Expr::Index {
                    base: base.clone(),
                    index: index.clone(),
                })?;
                Ok(self.c_load(&p, ty))
            }
            Expr::Index { base, index } => self.eval_index(base, index),
            Expr::Slice { base, from, to } => self.eval_slice(base, from.as_deref(), to.as_deref()),
            // `1 <= x <= 12` and `e in xs` are sugar that only becomes a
            // concrete command once the operand types are known — which they are
            // here. Both lower to expressions built entirely from nodes that
            // already exist, so the work is one more `eval`.
            Expr::Chain {
                lo,
                lo_op,
                mid,
                hi_op,
                hi,
            } => self.eval_chain(lo, *lo_op, mid, *hi_op, hi),
            Expr::In {
                needle,
                haystack,
                negated,
            } => self.eval_in(needle, haystack, *negated),
            // One interpolation hole. The value is rendered to text by the very
            // routine that renders a value assigned to a component property, so
            // a bool is `true`/`false` and a number goes through its
            // `*_to_text` — the checker has already refused a type with no text
            // form, naming the hole.
            Expr::ToText { value, .. } => {
                let v = self.eval(value)?;
                let operand = self.value_as_text(&v)?;
                Ok(Val {
                    ty: Ty::Text,
                    operand,
                })
            }
            Expr::IntLit(v) => {
                if let Ok(v32) = i32::try_from(*v) {
                    Ok(Val {
                        ty: Ty::Int,
                        operand: v32.to_string(),
                    })
                } else {
                    Ok(Val {
                        ty: Ty::Int64,
                        operand: v.to_string(),
                    })
                }
            }
            Expr::BitsLit(v) => {
                let ty = kiln_ir::sema::bits_bare_type(*v);
                Ok(Val {
                    ty,
                    operand: kiln_ir::sema::bits_value(*v).to_string(),
                })
            }
            Expr::DoubleLit(v) => Ok(Val {
                ty: Ty::Double,
                operand: format!("0x{:016X}", v.to_bits()),
            }),
            Expr::TextLit(s) => {
                let id = self.strings.len();
                self.strings.push(s.clone());
                let bytes = s.len() + 1;
                Ok(Val {
                    ty: Ty::Text,
                    operand: format!(
                        "getelementptr inbounds ([{bytes} x i8], ptr @.str{id}, i64 0, i64 0)"
                    ),
                })
            }
            Expr::Var(name) => {
                if let Some((slot, ty)) = self.vars.get(name).cloned() {
                    // A c-record local IS its storage: the name evaluates to the
                    // address of the flat struct, not a value loaded out of a
                    // slot. That address is what a field GEP walks, what `dll`
                    // passes for a struct pointer, and what `address of` hands
                    // to C — so there is nothing to load, and loading would read
                    // the first bytes of the struct as if they were a pointer.
                    if let Ty::Record(rec) = ty {
                        if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                            return Ok(Val { ty, operand: slot });
                        }
                    }
                    // A value that may be absent is not a value yet. The
                    // checker refuses every reading of one that has not been
                    // unwrapped, so this is unreachable from a program that
                    // type-checked — and if it ever were reached, a silent load
                    // would hand back a value nothing had stored.
                    if let Ty::Optional(_) = ty {
                        return err(format!(
                            "`{name}` may hold no value — read it with `{name} otherwise ...` or \
                             `if some {name} as ...`"
                        ));
                    }
                    let t = self.fresh();
                    writeln!(self.body, "  {t} = load {}, ptr {slot}", llvm_ty(ty)).unwrap();
                    Ok(Val { ty, operand: t })
                } else if let Some(ty) = self.globals.get(name).copied() {
                    let t = self.fresh();
                    writeln!(
                        self.body,
                        "  {t} = load {}, ptr @{}",
                        llvm_ty(ty),
                        global_symbol(name)
                    )
                    .unwrap();
                    Ok(Val { ty, operand: t })
                } else {
                    Err(LowerError {
                        msg: format!("use of undefined variable `{name}`"),
                    })
                }
            }
            Expr::Bin(op, l, r) => {
                let lv = self.eval(l)?;
                let rv = self.eval(r)?;
                // `+` on text is concatenation, not arithmetic: it forwards to
                // the same `concat` command an author could call by name.
                if lv.ty == Ty::Text && rv.ty == Ty::Text && *op == BinOp::Add {
                    let t = self.call_symbol_2("kn_concat", &lv, &rv, Ty::Text)?;
                    return Ok(Val {
                        ty: Ty::Text,
                        operand: t,
                    });
                }
                // `text * count` repeats the text: it forwards to the `repeat`
                // command, the same operation an author could call by name. The
                // checker has already required the text on the left and an `int`
                // count on the right.
                if lv.ty == Ty::Text && rv.ty == Ty::Int && *op == BinOp::Mul {
                    let t = self.call_symbol_2("kn_repeat", &lv, &rv, Ty::Text)?;
                    return Ok(Val {
                        ty: Ty::Text,
                        operand: t,
                    });
                }
                if lv.ty != rv.ty || !lv.ty.is_numeric() {
                    return err("arithmetic requires matching numeric operands");
                }
                // Integer division and remainder trap on the two inputs the
                // hardware cannot answer. Without this the process dies of
                // SIGFPE with nothing said; the runtime has an error channel,
                // so use it.
                if matches!(op, BinOp::Div | BinOp::Rem) && lv.ty != Ty::Double {
                    self.guard_divisor(*op, &lv, &rv)?;
                }
                let opcode = match (op, lv.ty) {
                    (BinOp::Add, Ty::Double) => "fadd",
                    (BinOp::Sub, Ty::Double) => "fsub",
                    (BinOp::Mul, Ty::Double) => "fmul",
                    (BinOp::Div, Ty::Double) => "fdiv",
                    (BinOp::Rem, Ty::Double) => "frem",
                    (BinOp::Add, _) => "add",
                    (BinOp::Sub, _) => "sub",
                    (BinOp::Mul, _) => "mul",
                    (BinOp::Div, _) => "sdiv",
                    (BinOp::Rem, _) => "srem",
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
            Expr::Call { cmd, args } => {
                let v = self.eval_call(cmd, args)?;
                v.ok_or_else(|| LowerError {
                    msg: format!("command `{cmd}` returns nothing and cannot be used as a value"),
                })
            }
            Expr::BoolLit(b) => Ok(Val {
                ty: Ty::Bool,
                operand: (*b as i32).to_string(),
            }),
            Expr::Neg(e) => {
                let v = self.eval(e)?;
                if !v.ty.is_numeric() {
                    return err(format!("`-` negates numbers, got {}", v.ty.as_str()));
                }
                let t = self.fresh();
                if v.ty == Ty::Double {
                    writeln!(self.body, "  {t} = fneg double {}", v.operand).unwrap();
                } else {
                    writeln!(self.body, "  {t} = sub {} 0, {}", llvm_ty(v.ty), v.operand).unwrap();
                }
                Ok(Val {
                    ty: v.ty,
                    operand: t,
                })
            }
            Expr::Not(e) => {
                let v = self.eval(e)?;
                let t = self.fresh();
                writeln!(self.body, "  {t} = xor i32 {}, 1", v.operand).unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            // `address of NAME` — the subroutine's own function symbol, as a
            // `ptr`. Under opaque pointers a function is already a `ptr`-typed
            // constant, so there is no bitcast to emit (the C mental model of
            // "cast the function pointer to void*" is a no-op here); the bare
            // `@kn_user_<name>` constant is a valid operand in every position a
            // `Val` is spliced into. The checker has proven `NAME` is a sub with
            // a C-representable signature, so the symbol both exists (all subs
            // are emitted) and is callable across the C ABI. The reference is a
            // relocation from this — reachable — function to the sub's own
            // section, which is exactly what keeps `--gc-sections` from dropping
            // a sub whose address is taken but which nothing calls directly, the
            // same way an event handler's thunk keeps its handler alive.
            Expr::AddressOf(name) => {
                // `address of r.pt` / `address of r.rgb` — the address of that
                // field inside the struct's own storage (for an inline array,
                // of its first element, which is where C's own `&r.rgb` points).
                if let Some((root, rest)) = name.split_once('.') {
                    let mut place = Expr::Var(root.to_string());
                    for step in rest.split('.') {
                        place = Expr::Field {
                            base: Box::new(place),
                            name: step.to_string(),
                        };
                    }
                    let (p, _) = self.c_place_ptr(&place)?;
                    return Ok(Val {
                        ty: Ty::Ptr,
                        operand: p,
                    });
                }
                // `address of r` for a c-record local is that local's own
                // address — the pointer a C API is handed. A c-record `Var`
                // already evaluates to its address, so this is the same operand,
                // typed `ptr`. Otherwise it is a subroutine's function symbol
                // (see the note below), which the checker has proven callable.
                if let Some((slot, Ty::Record(rec))) = self.vars.get(name).cloned() {
                    if self.reg.record(rec).map(|d| d.is_c).unwrap_or(false) {
                        return Ok(Val {
                            ty: Ty::Ptr,
                            operand: slot,
                        });
                    }
                }
                // The sub's own convention marker (`Sub::conv`), if any, is not
                // encoded on this function pointer for the same reason a `dll`'s
                // is not: one C convention per 64-bit target. A 32-bit backend
                // would read it from the `Sub` AST node when emitting the sub's
                // definition, not here.
                Ok(Val {
                    ty: Ty::Ptr,
                    operand: format!("@{}", user_symbol(name)),
                })
            }
            // `call through EXPR(args...): T` in a value position. The checker
            // has proven the callee is a `ptr` and that the site declares a
            // return type, so the result is always there to hand back.
            Expr::CallThrough {
                callee,
                args,
                ret,
                conv,
            } => match self.eval_call_through(callee, args, *ret, *conv)? {
                Some(v) => Ok(v),
                None => err("a `call through` with no return type has no value".to_string()),
            },
            // `size of TYPE` is a compile-time constant: a c-record's flat
            // `sizeof`, or a scalar's C width. The checker has already agreed the
            // type has one, so this folds to the number with no runtime cost.
            Expr::SizeOf(t) => {
                let size = match t {
                    Ty::Record(rec) => self.c_record_size(rec)?,
                    other => other
                        .c_size_align(self.machine)
                        .map(|(s, _)| s)
                        .ok_or_else(|| LowerError {
                            msg: format!("`size of {}` has no C layout", other.as_str()),
                        })?,
                };
                Ok(Val {
                    ty: Ty::Int64,
                    operand: size.to_string(),
                })
            }
            // A bare `ZeroInit` never reaches here: it is the initializer of a
            // c-record `var`, consumed by the `Let` arm before any value is
            // evaluated. Reaching it means the checker let one through somewhere
            // it should not have.
            Expr::ZeroInit => {
                err("an uninitialised c-record value is only valid as `var r: RECT`".to_string())
            }
            Expr::Cmp(op, l, r) => {
                let lv = self.eval(l)?;
                let rv = self.eval(r)?;
                if lv.ty == Ty::Text {
                    // Text comparison must compare CONTENT, not pointers.
                    let eq = self.call_text_eq(&lv, &rv)?;
                    return match op {
                        CmpOp::Eq => Ok(Val {
                            ty: Ty::Bool,
                            operand: eq,
                        }),
                        CmpOp::Ne => {
                            let t = self.fresh();
                            writeln!(self.body, "  {t} = xor i32 {eq}, 1").unwrap();
                            Ok(Val {
                                ty: Ty::Bool,
                                operand: t,
                            })
                        }
                        _ => err("text values support only `=` and `<>`"),
                    };
                }
                let pred = match (op, lv.ty) {
                    (CmpOp::Eq, Ty::Double) => "fcmp oeq",
                    (CmpOp::Ne, Ty::Double) => "fcmp one",
                    (CmpOp::Lt, Ty::Double) => "fcmp olt",
                    (CmpOp::Le, Ty::Double) => "fcmp ole",
                    (CmpOp::Gt, Ty::Double) => "fcmp ogt",
                    (CmpOp::Ge, Ty::Double) => "fcmp oge",
                    (CmpOp::Eq, _) => "icmp eq",
                    (CmpOp::Ne, _) => "icmp ne",
                    (CmpOp::Lt, _) => "icmp slt",
                    (CmpOp::Le, _) => "icmp sle",
                    (CmpOp::Gt, _) => "icmp sgt",
                    (CmpOp::Ge, _) => "icmp sge",
                };
                let bit = self.fresh();
                writeln!(
                    self.body,
                    "  {bit} = {pred} {} {}, {}",
                    llvm_ty(lv.ty),
                    lv.operand,
                    rv.operand
                )
                .unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = zext i1 {bit} to i32").unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            Expr::Logical(op, l, r) => {
                // Short-circuit: the right side is evaluated only when needed,
                // so `x > 0 and 100 / x > 2` is safe.
                let slot = self.alloca(Ty::Bool);
                let rhs_label = self.fresh_label("rhs");
                let done = self.fresh_label("logic");
                let lv = self.eval(l)?;
                writeln!(self.body, "  store i32 {}, ptr {slot}", lv.operand).unwrap();
                let c = self.fresh();
                writeln!(self.body, "  {c} = icmp ne i32 {}, 0", lv.operand).unwrap();
                match op {
                    LogicalOp::And => {
                        writeln!(self.body, "  br i1 {c}, label %{rhs_label}, label %{done}")
                            .unwrap()
                    }
                    LogicalOp::Or => {
                        writeln!(self.body, "  br i1 {c}, label %{done}, label %{rhs_label}")
                            .unwrap()
                    }
                }
                writeln!(self.body, "{rhs_label}:").unwrap();
                let rv = self.eval(r)?;
                writeln!(self.body, "  store i32 {}, ptr {slot}", rv.operand).unwrap();
                writeln!(self.body, "  br label %{done}").unwrap();
                writeln!(self.body, "{done}:").unwrap();
                let t = self.fresh();
                writeln!(self.body, "  {t} = load i32, ptr {slot}").unwrap();
                Ok(Val {
                    ty: Ty::Bool,
                    operand: t,
                })
            }
            Expr::GetProperty {
                component,
                property,
            } => {
                // Variables first, then component ids — the order the checker
                // resolved it in, or the two would lower different programs.
                if let Some(Ty::Record(rec)) = self.var_ty(component) {
                    let b = self.eval(&Expr::Var(component.clone()))?;
                    return self.emit_field_read(rec, &b, property);
                }
                let handle = self.handle_of(component)?;
                let n = self.cstr(property);
                let ty = self.property_ty(component, property)?;
                let lib = self.owner(component);
                match ty {
                    Ty::Int => {
                        let f = match &lib {
                            None => {
                                self.ui_used.insert("kn_ui_get_int");
                                "kn_ui_get_int".to_string()
                            }
                            Some(lib) => {
                                self.component_libs.insert(lib.clone());
                                format!("kn_{lib}_component_get_int")
                            }
                        };
                        let t = self.fresh();
                        writeln!(self.body, "  {t} = call i32 @{f}(i64 {handle}, ptr {n})")
                            .unwrap();
                        Ok(Val {
                            ty: Ty::Int,
                            operand: t,
                        })
                    }
                    // A truth value has no getter of its own: every
                    // implementation of the property ABI answers a bool as the
                    // text `true` or `false`, so the read is the text read and
                    // the comparison is what makes it a bool again. Without
                    // this arm the value falls through as text and `if
                    // agree.checked` fails to lower at all.
                    Ty::Bool => {
                        let f = match &lib {
                            None => {
                                self.ui_used.insert("kn_ui_get");
                                "kn_ui_get".to_string()
                            }
                            Some(lib) => {
                                self.component_libs.insert(lib.clone());
                                format!("kn_{lib}_component_get")
                            }
                        };
                        let t = self.fresh();
                        writeln!(self.body, "  {t} = call ptr @{f}(i64 {handle}, ptr {n})")
                            .unwrap();
                        let read = Val {
                            ty: Ty::Text,
                            operand: t,
                        };
                        let yes = self.eval(&Expr::TextLit("true".into()))?;
                        let eq = self.call_text_eq(&read, &yes)?;
                        Ok(Val {
                            ty: Ty::Bool,
                            operand: eq,
                        })
                    }
                    _ => {
                        let f = match &lib {
                            None => {
                                self.ui_used.insert("kn_ui_get");
                                "kn_ui_get".to_string()
                            }
                            Some(lib) => {
                                self.component_libs.insert(lib.clone());
                                format!("kn_{lib}_component_get")
                            }
                        };
                        let t = self.fresh();
                        writeln!(self.body, "  {t} = call ptr @{f}(i64 {handle}, ptr {n})")
                            .unwrap();
                        Ok(Val {
                            ty: Ty::Text,
                            operand: t,
                        })
                    }
                }
            }
        }
    }
}
