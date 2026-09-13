//! Phase 1 exit fixtures (design/k2/kir.md §"exit fixtures").
//!
//! Hand-written KIR — no parser yet — that must compile with clang and run,
//! proving KIR carries what K2 needs and 1.x cannot express: a by-reference
//! closure, monomorphised generics with mangled symbols, a method call through
//! a `this` pointer, and unsigned arithmetic. Each fixture links against libc
//! only (printf/malloc), so the test needs no Kiln runtime.

use kiln_kir::build::ModuleBuilder;
use kiln_kir::*;
use std::process::Command as Proc;

/// Emit, compile with clang, run, and return stdout.
fn run(m: &Module) -> String {
    let ll = emit::emit(m);
    let dir = std::env::temp_dir().join(format!("kir-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let name = m.name.clone();
    let ll_path = dir.join(format!("{name}.ll"));
    let exe_path = dir.join(&name);
    std::fs::write(&ll_path, &ll).unwrap();

    // A tiny C main that calls the module's ECodeStart, so we link with libc's
    // crt and printf resolves. Kiln's real entry is ECodeStart.
    let shim = dir.join("shim.c");
    std::fs::write(
        &shim,
        "extern int ECodeStart(void); int main(void){return ECodeStart();}\n",
    )
    .unwrap();

    let out = Proc::new("clang")
        .args([
            ll_path.to_str().unwrap(),
            shim.to_str().unwrap(),
            "-o",
            exe_path.to_str().unwrap(),
        ])
        .output()
        .expect("clang runs");
    assert!(
        out.status.success(),
        "clang failed for {name}:\n{}\n--- IR ---\n{ll}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Proc::new(&exe_path).output().expect("fixture runs");
    assert!(
        run.status.success(),
        "{name} exited {:?}",
        run.status.code()
    );
    String::from_utf8(run.stdout).unwrap()
}

/// A printf("%d\n", value) statement over an i32 expression.
fn print_i32(value: Expr) -> Stmt {
    Stmt::Expr(Expr::Call(Box::new(Call::Dll {
        library: "c".into(),
        symbol: "printf".into(),
        conv: CallConv::Cdecl,
        args: vec![Expr::Str("%d\n".into()), value],
        arg_tys: vec![TyTable::STR],
        ret: TyTable::I32,
        varargs: true,
    })))
}

/// A printf("%u\n", value) statement over a u32 expression.
fn print_u32(value: Expr) -> Stmt {
    Stmt::Expr(Expr::Call(Box::new(Call::Dll {
        library: "c".into(),
        symbol: "printf".into(),
        conv: CallConv::Cdecl,
        args: vec![Expr::Str("%u\n".into()), value],
        arg_tys: vec![TyTable::STR],
        ret: TyTable::I32,
        varargs: true,
    })))
}

// 1 ─ Closure with a captured environment, captured BY REFERENCE.
#[test]
fn closure_captures_by_reference() {
    let mut b = ModuleBuilder::new("closure", ModuleKind::Console, Target::X86_64_LINUX);
    // An env record holding the captured `n`.
    let env_rec = b.c_record("Env", vec![("n", TyTable::I32)], Equality::ByRef);
    let env_ty = b.record_ty(env_rec);
    let clos_ty = b.func_ty(vec![], TyTable::I32);

    // The lambda: fn(env) -> i32 { return env.n }
    let lam = b.declare_func("lam", vec![("env", env_ty)], TyTable::I32);
    b.set_body(
        lam,
        vec![Stmt::Return(Some(Expr::Field(
            Box::new(Expr::Local(b.param_local(0))),
            0,
        )))],
    );

    // main: make env{n=10}, close over it, mutate n to 42, then call — sees 42.
    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let env_l = b.add_local(main, "env", env_ty);
    let clos_l = b.add_local(main, "clos", clos_ty);
    b.set_body(
        main,
        vec![
            Stmt::Let {
                local: env_l,
                value: Expr::MakeRecord(env_rec, vec![Expr::Int(10, TyTable::I32)]),
            },
            Stmt::Let {
                local: clos_l,
                value: Expr::MakeClosure {
                    func: lam,
                    env: Box::new(Expr::Local(env_l)),
                },
            },
            // Mutate the captured variable AFTER the closure is built.
            Stmt::Assign {
                place: Place::Field(Box::new(Expr::Local(env_l)), 0),
                value: Expr::Int(42, TyTable::I32),
            },
            print_i32(Expr::Call(Box::new(Call::Indirect {
                callee: Box::new(Expr::Local(clos_l)),
                args: vec![],
                sig: clos_ty,
            }))),
            Stmt::Return(None),
        ],
    );
    b.set_entry(main);
    assert_eq!(run(&b.build()), "42\n");
}

// 2 ─ Monomorphised generic: Max$i32 and Max$f64, distinct mangled symbols.
#[test]
fn monomorphised_generic_instances() {
    let mut b = ModuleBuilder::new("generic", ModuleKind::Console, Target::X86_64_LINUX);

    let max_i = b.declare_func(
        "K2Max$i32",
        vec![("a", TyTable::I32), ("b", TyTable::I32)],
        TyTable::I32,
    );
    b.set_body(
        max_i,
        vec![Stmt::If {
            cond: Expr::Bin(
                BinOp::Ge,
                Box::new(Expr::Local(b.param_local(0))),
                Box::new(Expr::Local(b.param_local(1))),
                TyTable::I32,
            ),
            then: vec![Stmt::Return(Some(Expr::Local(b.param_local(0))))],
            els: vec![Stmt::Return(Some(Expr::Local(b.param_local(1))))],
        }],
    );

    let max_f = b.declare_func(
        "K2Max$f64",
        vec![("a", TyTable::F64), ("b", TyTable::F64)],
        TyTable::F64,
    );
    b.set_body(
        max_f,
        vec![Stmt::If {
            cond: Expr::Bin(
                BinOp::Ge,
                Box::new(Expr::Local(b.param_local(0))),
                Box::new(Expr::Local(b.param_local(1))),
                TyTable::F64,
            ),
            then: vec![Stmt::Return(Some(Expr::Local(b.param_local(0))))],
            els: vec![Stmt::Return(Some(Expr::Local(b.param_local(1))))],
        }],
    );

    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let s1 = print_i32(Expr::Call(Box::new(Call::Direct {
        func: max_i,
        args: vec![Expr::Int(3, TyTable::I32), Expr::Int(7, TyTable::I32)],
    })));
    // Max$f64(2.5, 1.5) == 2.5; cast to i32 (== 2) so we can print it plainly.
    let s2 = print_i32(Expr::Cast {
        value: Box::new(Expr::Call(Box::new(Call::Direct {
            func: max_f,
            args: vec![
                Expr::Float(2.5, TyTable::F64),
                Expr::Float(1.5, TyTable::F64),
            ],
        }))),
        to: TyTable::I32,
    });
    b.set_body(main, vec![s1, s2, Stmt::Return(None)]);
    b.set_entry(main);
    assert_eq!(run(&b.build()), "7\n2\n");
}

// 3 ─ A method call: Area(this: Rect) through an explicit `this` first param.
#[test]
fn method_call_through_this() {
    let mut b = ModuleBuilder::new("method", ModuleKind::Console, Target::X86_64_LINUX);
    let rect = b.c_record(
        "Rect",
        vec![("w", TyTable::I32), ("h", TyTable::I32)],
        Equality::ByValue,
    );
    let rect_ty = b.record_ty(rect);

    let area = b.declare_func("Rect_Area", vec![("this", rect_ty)], TyTable::I32);
    b.set_body(
        area,
        vec![Stmt::Return(Some(Expr::Bin(
            BinOp::Mul,
            Box::new(Expr::Field(Box::new(Expr::Local(b.param_local(0))), 0)),
            Box::new(Expr::Field(Box::new(Expr::Local(b.param_local(0))), 1)),
            TyTable::I32,
        )))],
    );

    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let r = b.add_local(main, "r", rect_ty);
    let call = print_i32(Expr::Call(Box::new(Call::Direct {
        func: area,
        args: vec![Expr::Local(r)],
    })));
    b.set_body(
        main,
        vec![
            Stmt::Let {
                local: r,
                value: Expr::MakeRecord(
                    rect,
                    vec![Expr::Int(3, TyTable::I32), Expr::Int(4, TyTable::I32)],
                ),
            },
            call,
            Stmt::Return(None),
        ],
    );
    b.set_entry(main);
    assert_eq!(run(&b.build()), "12\n");
}

// 4 ─ Unsigned arithmetic: udiv, icmp ult, lshr on values above the signed range.
#[test]
fn unsigned_arithmetic() {
    let mut b = ModuleBuilder::new("unsigned", ModuleKind::Console, Target::X86_64_LINUX);
    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let max = 0xFFFF_FFFFi128; // u32 4294967295; as i32 that is -1

    // udiv: 0xFFFFFFFF / 2 == 2147483647 (sdiv would give 0)
    let udiv = print_u32(Expr::Bin(
        BinOp::Div,
        Box::new(Expr::Int(max, TyTable::U32)),
        Box::new(Expr::Int(2, TyTable::U32)),
        TyTable::U32,
    ));
    // icmp ult: (0xFFFFFFFF < 1) == false (slt would be true); print as 0/1
    let ult = print_i32(Expr::Cast {
        value: Box::new(Expr::Bin(
            BinOp::Lt,
            Box::new(Expr::Int(max, TyTable::U32)),
            Box::new(Expr::Int(1, TyTable::U32)),
            TyTable::U32,
        )),
        to: TyTable::I32,
    });
    // lshr: 0xFFFFFFFF >> 1 == 2147483647 (ashr would give 0xFFFFFFFF)
    let lshr = print_u32(Expr::Bin(
        BinOp::Shr,
        Box::new(Expr::Int(max, TyTable::U32)),
        Box::new(Expr::Int(1, TyTable::U32)),
        TyTable::U32,
    ));
    b.set_body(main, vec![udiv, ult, lshr, Stmt::Return(None)]);
    b.set_entry(main);
    assert_eq!(run(&b.build()), "2147483647\n0\n2147483647\n");
}

// 5 ─ Slot-ABI command marshalling (text-checked: no runtime linked here).
#[test]
fn command_marshals_over_the_slot_abi() {
    let mut b = ModuleBuilder::new("cmd", ModuleKind::Console, Target::X86_64_LINUX);
    let main = b.declare_func("main_", vec![], TyTable::VOID);
    // print_text(x): a void command taking one text arg (SDT_TEXT == 9).
    let call = Stmt::Expr(Expr::Call(Box::new(Call::Command {
        symbol: "kn_print_text".into(),
        args: vec![Expr::Str("hi".into())],
        arg_slots: vec![SlotTy {
            tag: 9,
            ty: TyTable::STR,
        }],
        ret: TyTable::VOID,
    })));
    b.set_body(main, vec![call, Stmt::Return(None)]);
    b.set_entry(main);
    let ll = emit::emit(&b.build());
    assert!(
        ll.contains("declare void @kn_print_text(ptr, i32, ptr)"),
        "{ll}"
    );
    assert!(ll.contains("store i32 9, ptr"), "tag stored\n{ll}");
    assert!(
        ll.contains("call void @kn_print_text(ptr %s"),
        "slot-ABI call\n{ll}"
    );
    assert!(
        ll.contains("ptrtoint ptr @.str0 to i64"),
        "text arg reinterpreted\n{ll}"
    );
}

// 6 ─ Widening a bool must zero-extend: `true` is 1, never -1.
#[test]
fn a_bool_widens_to_one_not_minus_one() {
    let mut b = ModuleBuilder::new("boolcast", ModuleKind::Console, Target::X86_64_LINUX);
    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let t = print_i32(Expr::Cast {
        value: Box::new(Expr::Bool(true)),
        to: TyTable::I32,
    });
    let f = print_i32(Expr::Cast {
        value: Box::new(Expr::Bool(false)),
        to: TyTable::I32,
    });
    b.set_body(main, vec![t, f, Stmt::Return(None)]);
    b.set_entry(main);
    assert_eq!(run(&b.build()), "1\n0\n");
}

// 7 ─ A runtime array must carry the tag and length the runtime expects.
//
// `kn_ary_new(tag, len)` takes the tag first. Swapped, an empty literal asks
// for an array of `tag` elements with no element type — and the failure lands
// far away, in whatever command reads it. This checks the emitted call rather
// than running it, since the array lives in the runtime and these fixtures
// link libc alone.
#[test]
fn an_array_is_built_with_the_tag_first_and_one_based_positions() {
    let mut b = ModuleBuilder::new("arylit", ModuleKind::Console, Target::X86_64_LINUX);
    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let arr = Expr::MakeArray(
        TyTable::I32,
        vec![Expr::Int(10, TyTable::I32), Expr::Int(20, TyTable::I32)],
    );
    let ty = b.m.types.intern(TyKind::Array(TyTable::I32));
    let l = b.add_local(main, "a", ty);
    b.set_body(main, vec![Stmt::Let { local: l, value: arr }, Stmt::Return(None)]);
    b.set_entry(main);
    let ll = kiln_kir::emit::emit(&b.build());
    // Tag 3 is KN_SDT_INT; the length is 2.
    assert!(
        ll.contains("@kn_ary_new(i32 3, i32 2)"),
        "the tag must come first:\n{ll}"
    );
    // And positions count from 1, as everywhere else in Kiln. The values
    // themselves go through temporaries, so only the position is asserted.
    assert!(ll.contains("@kn_ary_set(ptr %t0, i32 1,"), "{ll}");
    assert!(ll.contains("@kn_ary_set(ptr %t0, i32 2,"), "{ll}");
    assert!(
        !ll.contains("@kn_ary_set(ptr %t0, i32 0,"),
        "a zero-based store:\n{ll}"
    );
}

// 8 ─ An empty array is length 0, not length `tag`.
#[test]
fn an_empty_array_has_no_elements() {
    let mut b = ModuleBuilder::new("aryempty", ModuleKind::Console, Target::X86_64_LINUX);
    let main = b.declare_func("main_", vec![], TyTable::VOID);
    let ty = b.m.types.intern(TyKind::Array(TyTable::STR));
    let l = b.add_local(main, "a", ty);
    b.set_body(
        main,
        vec![
            Stmt::Let {
                local: l,
                value: Expr::MakeArray(TyTable::STR, vec![]),
            },
            Stmt::Return(None),
        ],
    );
    b.set_entry(main);
    let ll = kiln_kir::emit::emit(&b.build());
    // Tag 9 is KN_SDT_TEXT.
    assert!(ll.contains("@kn_ary_new(i32 9, i32 0)"), "{ll}");
}
