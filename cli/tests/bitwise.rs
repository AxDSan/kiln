//! Bitwise operators and hex/binary literals, end to end.
//!
//! The two halves arrived together because they are one gap: before this, a
//! flag word could only be combined with `+` (which is wrong the moment a flag
//! is set twice), a flag could not be *tested* at all, LOWORD and HIWORD were
//! division tricks, and every one of the win kit's ~880 constants was a
//! decimal with the hex in a comment beside it.
//!
//! `examples/bitwise.kiln` is a self-checking transcript: every line prints `ok`
//! or `FAIL`. The tests below run it and then pin the handful of answers that
//! would be silently wrong rather than loudly wrong if a rule slipped — the
//! width of a literal, the sign of a shift, the precedence of a flag test —
//! plus the diagnostics for the operands these operators do not have.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_bitwise_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn build(src: &Path, out: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build")
}

/// Build a one-off `main` whose body is `body`, and answer what the compiler
/// said. Used by the diagnostic tests, which care about the message.
fn build_body(dir: &Path, tag: &str, body: &str) -> (bool, String) {
    let src = dir.join(format!("{tag}.kiln"));
    std::fs::write(&src, format!("module t\nsub main\n{body}\nend\n")).expect("write the case");
    let out = build(&src, &dir.join(format!("{tag}.bin")));
    let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
    said.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), said)
}

/// Build a `main` that prints one expression, run it, and answer the line.
fn value_of(dir: &Path, tag: &str, decls: &str, printer: &str, expr: &str) -> String {
    let src = dir.join(format!("{tag}.kiln"));
    std::fs::write(
        &src,
        format!("module t\nsub main\n{decls}\n  call {printer}({expr})\nend\n"),
    )
    .expect("write the case");
    let bin = dir.join(format!("{tag}.bin"));
    let built = build(&src, &bin);
    assert!(
        built.status.success(),
        "`{expr}` did not build:\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&bin).output().expect("run the built binary");
    assert!(out.status.success(), "`{expr}` exited non-zero");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// --- the transcript ---------------------------------------------------------

/// The whole surface at once: `examples/bitwise.kiln` checks its own answers.
#[test]
fn the_bitwise_example_passes_its_own_checks() {
    let dir = scratch("example");
    let bin = dir.join("bitwise");
    let built = build(&repo().join("examples/bitwise.kiln"), &bin);
    assert!(
        built.status.success(),
        "examples/bitwise.kiln did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&bin).output().expect("run the example");
    assert!(out.status.success(), "the example exited non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let failures: Vec<&str> = stdout.lines().filter(|l| l.contains("FAIL")).collect();
    assert!(failures.is_empty(), "the example reported failures:\n{}", failures.join("\n"));
    assert!(!stdout.trim().is_empty(), "the example produced no output");
}

// --- literals ---------------------------------------------------------------

/// A hex or binary literal is a **bit pattern**, and its width comes from where
/// it lands. On its own a pattern of 32 bits or fewer is an `int` holding
/// exactly those bits — so `0x8000_0000` is the most negative `int` and
/// `0xFFFF_FFFF` is -1, which is what a mask written for a 32-bit word means.
#[test]
fn a_hex_literal_is_the_bits_that_were_written() {
    let dir = scratch("lits");
    for (expr, want) in [
        ("0xFF", "255"),
        ("0xff", "255"),
        ("0b1010", "10"),
        ("0xDEAD_BEEF", "-559038737"),
        ("0x80000000", "-2147483648"),
        ("0xFFFF_FFFF", "-1"),
        ("-0x10", "-16"),
    ] {
        let tag = format!("l{}", want.replace('-', "n"));
        assert_eq!(value_of(&dir, &tag, "", "print_int", expr), want, "for `{expr}`");
    }
}

/// The same pattern where an `int64` is wanted: 64 bits, gaining zeros rather
/// than a sign. This is the reading a Win32 `DWORD` constant needs —
/// `HKEY_CLASSES_ROOT` is `0x8000_0000`, and as an `int64` it is 2147483648.
#[test]
fn a_hex_literal_widens_with_zeros() {
    let dir = scratch("wide");
    assert_eq!(
        value_of(&dir, "w1", "  var v: int64 = 0x8000_0000", "print_int64", "v"),
        "2147483648"
    );
    // The same 32 bits, read as 64: 0xDEAD_BEEF is -559038737 as an `int` and
    // 3735928559 here. Width comes from the destination, not the digit count —
    // `0x0000_0000_DEAD_BEEF` is still 32 bits' worth and still an `int` bare.
    assert_eq!(
        value_of(&dir, "w1b", "  var v: int64 = 0xDEAD_BEEF", "print_int64", "v"),
        "3735928559"
    );
    // A pattern wider than 32 bits is an int64 on its own.
    assert_eq!(
        value_of(&dir, "w2", "  var v: int64 = 0x1_0000_0000", "print_int64", "v"),
        "4294967296"
    );
    assert_eq!(
        value_of(&dir, "w3", "  var v: int64 = 0xFFFF_FFFF_FFFF_FFFF", "print_int64", "v"),
        "-1"
    );
    // A constant is its literal, so it widens the same way.
    let src = dir.join("w4.kiln");
    std::fs::write(
        &src,
        "module t\nconst HKEY_CLASSES_ROOT = 0x8000_0000\nsub main\n  \
         var k: int64 = HKEY_CLASSES_ROOT\n  call print_int64(k)\n  \
         call print_int(HKEY_CLASSES_ROOT)\nend\n",
    )
    .unwrap();
    let bin = dir.join("w4.bin");
    assert!(build(&src, &bin).status.success());
    let out = Command::new(&bin).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "2147483648\n-2147483648",
        "a constant must read both ways, exactly as the literal does"
    );
}

/// A literal that is not one: no digits, a digit the base does not have, and a
/// pattern with no type wide enough to hold it.
#[test]
fn a_malformed_bit_pattern_is_refused() {
    let dir = scratch("badlit");
    for (body, needle) in [
        ("  call print_int(0x)", "has no digits"),
        ("  call print_int(0b1012)", "not a binary digit"),
        ("  call print_int(0xG1)", "not a hexadecimal digit"),
        ("  call print_int(0xFFFF_FFFF_FFFF_FFFF_F)", "more than 64 bits"),
    ] {
        let (ok, said) = build_body(&dir, "bad", body);
        assert!(!ok, "`{body}` built, and should not have");
        assert!(said.contains(needle), "wanted {needle:?} for `{body}`, got:\n{said}");
    }
}

/// A form or component property value must be a literal — and a bit pattern is
/// one. `width = 0x1E0` has to reach the UI layer as 480, not as "that is not
/// a literal".
///
/// Checked with `emit` rather than `build` so it runs on a machine without the
/// vendored UI stack: lowering is where the property is rendered.
#[test]
fn a_property_may_be_written_as_a_bit_pattern() {
    let dir = scratch("prop");
    let src = dir.join("form.kiln");
    std::fs::write(
        &src,
        "module t\nuse ui\nform w\n  title = \"x\"\n  width = 0x1E0\n           label g\n    text = \"hi\"\n    left = 0b1_0000\n  end\nend\nsub main\nend\n",
    )
    .unwrap();
    let emitted = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["emit", src.to_str().unwrap()])
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln emit");
    assert!(
        emitted.status.success(),
        "a form with hex properties did not lower:\n{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    // ...and `inspect`, which is how the designer reads a project, shows the
    // number rather than a blank.
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["inspect", src.to_str().unwrap()])
        .current_dir(repo())
        .output()
        .expect("run kiln inspect");
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(said.contains("prop: w width 480"), "inspect said:\n{said}");
    assert!(said.contains("prop: g left 16"), "inspect said:\n{said}");
}

// --- the operators ----------------------------------------------------------

#[test]
fn the_five_binary_operators_and_the_unary_one() {
    let dir = scratch("ops");
    for (tag, expr, want) in [
        ("and", "0xF0 band 0x0F", "0"),
        ("andk", "0xF3 band 0x0F", "3"),
        ("or", "0xF0 bor 0x0F", "255"),
        ("xor", "5 bxor 1", "4"),
        ("notz", "bnot 0", "-1"),
        ("not5", "bnot 5", "-6"),
        ("shl", "1 shl 4", "16"),
        ("shr", "256 shr 4", "16"),
    ] {
        assert_eq!(value_of(&dir, tag, "", "print_int", expr), want, "for `{expr}`");
    }
}

/// `shr` keeps the sign (it is an arithmetic shift) and `ushr` shifts zeros in.
/// The two differ only on a negative value, which is exactly where a program
/// that meant the other one goes quietly wrong.
#[test]
fn shr_keeps_the_sign_and_ushr_does_not() {
    let dir = scratch("sign");
    assert_eq!(value_of(&dir, "s1", "", "print_int", "-16 shr 2"), "-4");
    assert_eq!(value_of(&dir, "s2", "", "print_int", "-16 ushr 2"), "1073741820");
    assert_eq!(value_of(&dir, "s3", "  var v: int64 = -16", "print_int64", "v shr 2"), "-4");
    assert_eq!(
        value_of(&dir, "s4", "  var v: int64 = -16", "print_int64", "v ushr 2"),
        "4611686018427387900"
    );
}

/// A count is a count: it carries its own type, the result is the value's, and
/// a count the compiler can see must be in range. One computed at run time is
/// taken modulo the width, so a shift never produces a poison value.
#[test]
fn a_shift_count_is_checked_when_it_is_written_down() {
    let dir = scratch("count");
    // An `int` count shifting an `int64` needs no conversion.
    assert_eq!(
        value_of(&dir, "c1", "  var v: int64 = 1\n  var n: int = 40", "print_int64", "v shl n"),
        "1099511627776"
    );
    for (body, needle) in [
        ("  call print_int(1 shl 32)", "shifted by 0 to 31"),
        ("  call print_int(1 shl -1)", "shifted by 0 to 31"),
        ("  var v: int64 = 1\n  call print_int64(v shl 64)", "shifted by 0 to 63"),
    ] {
        let (ok, said) = build_body(&dir, "c", body);
        assert!(!ok, "`{body}` built, and should not have");
        assert!(said.contains(needle), "wanted {needle:?} for `{body}`, got:\n{said}");
    }
    // A count only known at run time is masked rather than left undefined.
    assert_eq!(value_of(&dir, "c2", "  var n: int = 33", "print_int", "1 shl n"), "2");
}

// --- precedence -------------------------------------------------------------

/// The table, checked by arithmetic rather than by reading the parser.
///
/// A bitwise operator binds **looser** than a comparison, so a flag test needs
/// no parentheses — the one place C's ordering is famously the wrong way round.
/// Shifts bind tighter than `band`/`bxor`/`bor`, and `+` tighter than a shift.
#[test]
fn precedence() {
    let dir = scratch("prec");
    for (tag, expr, want) in [
        ("p1", "1 shl 4 band 0xFF", "16"),         // (1 shl 4) band 0xFF
        ("p2", "1 shl 2 + 2", "16"),               // 1 shl (2 + 2)
        ("p3", "1 bor 6 band 4", "5"),             // 1 bor (6 band 4)
        ("p4", "1 bxor 3 band 1", "0"),            // 1 bxor (3 band 1)
        ("p5", "bnot 0 band 255", "255"),          // (bnot 0) band 255
        ("p6", "2 * 3 shl 1", "12"),               // (2 * 3) shl 1
    ] {
        assert_eq!(value_of(&dir, tag, "", "print_int", expr), want, "for `{expr}`");
    }
    // A comparison is looser than every bitwise operator: `6 band 4 = 4` is
    // `(6 band 4) = 4` and not `6 band (4 = 4)` — which would not even type.
    // `and`/`or` are looser still, so a two-flag test needs no parentheses
    // anywhere.
    let src = dir.join("p7.kiln");
    std::fs::write(
        &src,
        "module t\nsub main\n             if 6 band 4 = 4\n    call print_text(\"cmp is looser\")\n  end\n             if 6 band 4 <> 0 and 1 band 1 <> 0\n    call print_text(\"and is looser\")\n  end\n         end\n",
    )
    .unwrap();
    let bin = dir.join("p7.bin");
    let built = build(&src, &bin);
    assert!(
        built.status.success(),
        "the comparison-precedence program did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&bin).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "cmp is looser\nand is looser"
    );
}

// --- what this was for ------------------------------------------------------

/// The two shapes the win kit could not write: a WPARAM split into its halves,
/// and a style word tested for one flag.
///
/// `wparam` is an `int64` and `0xFFFF` is an `int` literal — a literal takes
/// the width of what it meets, so the mask needs no `int_to_int64`.
#[test]
fn loword_hiword_and_a_flag_test() {
    let dir = scratch("win32");
    let decls = "  var wparam: int64 = 196610";
    assert_eq!(value_of(&dir, "lo", decls, "print_int64", "wparam band 0xFFFF"), "2");
    assert_eq!(
        value_of(&dir, "hi", decls, "print_int64", "wparam ushr 16 band 0xFFFF"),
        "3"
    );

    let src = dir.join("flags.kiln");
    std::fs::write(
        &src,
        "module t\n\
         const WS_VISIBLE = 0x1000_0000\n\
         const WS_POPUP = 0x8000_0000\n\
         const WS_BORDER = 0x0080_0000\n\
         sub main\n  \
           var style: int = WS_VISIBLE bor WS_POPUP\n  \
           if style band WS_VISIBLE <> 0\n    call print_text(\"visible\")\n  end\n  \
           if style band WS_BORDER = 0\n    call print_text(\"no border\")\n  end\n  \
           call print_int(style band bnot WS_POPUP)\n\
         end\n",
    )
    .unwrap();
    let bin = dir.join("flags.bin");
    let built = build(&src, &bin);
    assert!(
        built.status.success(),
        "the flag program did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&bin).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "visible\nno border\n268435456",
        "combining flags with `bor`, testing one with `band`, clearing one with `bnot`"
    );
}

// --- diagnostics ------------------------------------------------------------

/// These operators are defined on `int` and `int64` and on nothing else. A
/// `double`'s bits are an IEEE encoding and `and`-ing two of them is never what
/// was meant; `bool`, `text` and `ptr` have no bits a program may address.
#[test]
fn a_non_integer_operand_is_named_and_refused() {
    let dir = scratch("types");
    for (body, needle) in [
        ("  call print_double(1.5 band 2.0)", "`band` works on int and int64 values; its left side is double"),
        ("  call print_int(1 bor 2.0)", "its right side is double"),
        ("  var b: bool = true\n  call print_int(b bxor 1)", "its left side is bool"),
        ("  call print_text(\"a\" bor \"b\")", "its left side is text"),
        ("  var p: ptr = ptr_null()\n  call print_int(p band 1)", "its left side is ptr"),
        ("  call print_double(bnot 1.5)", "`bnot` flips the bits of an int or an int64, got double"),
        ("  call print_int(1 shl 1.5)", "must be int or int64, not double"),
    ] {
        let (ok, said) = build_body(&dir, "t", body);
        assert!(!ok, "`{body}` built, and should not have");
        assert!(said.contains(needle), "wanted {needle:?} for `{body}`, got:\n{said}");
    }
}

/// An `int` **variable** is not widened to meet an `int64` — that would be the
/// implicit conversion the language does not have. Only a literal is, and the
/// message says which way out there is.
#[test]
fn a_mixed_pair_of_variables_is_refused_by_name() {
    let dir = scratch("mixed");
    let (ok, said) = build_body(
        &dir,
        "m",
        "  var a: int64 = 1\n  var b: int = 2\n  call print_int64(a band b)",
    );
    assert!(!ok, "a mixed pair built, and should not have");
    assert!(
        said.contains("same width") && said.contains("int_to_int64"),
        "the message should name the fix, got:\n{said}"
    );
    // ...and with the conversion written down, it builds.
    assert_eq!(
        value_of(
            &dir,
            "m2",
            "  var a: int64 = 3\n  var b: int = 1",
            "print_int64",
            "a band int_to_int64(b)"
        ),
        "1"
    );
}

/// The **infix** operator words are soft keywords: they are read as operators
/// only where an operator can go — after a complete operand, where an
/// identifier could never have appeared — so a program that used one as a name
/// keeps working.
///
/// `bnot` is the exception, and it is reserved. A prefix operator has no such
/// shelter: `bnot(x)` reads as the operator and as a call to a subroutine
/// named `bnot` equally well, and `bnot - 1` as a complement and as a
/// subtraction. Guessing there is a wrong *answer*, not an error, so the word
/// is refused as a name where it is written.
#[test]
fn the_operator_words_are_still_ordinary_names() {
    let dir = scratch("soft");
    assert_eq!(
        value_of(&dir, "n1", "  var band: int = 7", "print_int", "band + 1"),
        "8"
    );
    assert_eq!(
        value_of(&dir, "n2", "  var shl: int = 2\n  var shr: int = 3", "print_int", "shl * shr"),
        "6"
    );
    // A name and the operator of the same spelling, on one line.
    assert_eq!(
        value_of(&dir, "n2b", "  var shl: int = 7\n  var n: int = 2", "print_int", "shl shl n"),
        "28"
    );
    // ...and `bnot` is reserved, so a program that tried to use it as a name
    // is told where, rather than quietly meaning the operator.
    for body in [
        "  var bnot: int = 7\n  call print_int(bnot)",
        "  call print_int(1)\nend\nsub bnot(n: int): int\n  return n",
    ] {
        let (ok, said) = build_body(&dir, "n4", body);
        assert!(!ok, "`bnot` was accepted as a name");
        assert!(said.contains("BNot"), "the error should point at `bnot`, got:\n{said}");
    }
    // The operator itself, including in front of a parenthesised expression.
    assert_eq!(value_of(&dir, "n5", "", "print_int", "bnot (1 bor 2)"), "-4");
    // A field may be named for one too.
    let src = dir.join("field.kiln");
    std::fs::write(
        &src,
        "module t\nrecord flags\n  band: int\n  shl: int\nend\n\
         sub main\n  var f: flags = flags(band: 5, shl: 2)\n  call print_int(f.band shl f.shl)\nend\n",
    )
    .unwrap();
    let bin = dir.join("field.bin");
    let built = build(&src, &bin);
    assert!(
        built.status.success(),
        "a record with fields named for operators did not build:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let out = Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "20");
}
