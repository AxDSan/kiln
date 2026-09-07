//! End-to-end tests for the 0.9.0 expression sugar: the conditional value
//! (`if C then A else B`), the fallback (`E otherwise F`), the method spelling
//! of a call (`x.f(a)`), and `check`, which propagates a failed call out of the
//! subroutine it is in.
//!
//! Every one of these is a *rewrite* — into a branch the block `if` already
//! makes, into a call spelled the other way round, into the four lines a
//! program writes by hand around `last_error_code()`. So the tests prove the
//! OUTPUT: sugar that type-checks and computes the wrong thing is the failure
//! that matters. The build-error cases pin the two rules that must stay strict —
//! the arms of a conditional share one type, and a `.name` with no parentheses
//! is still a property read, not a call.
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_sugar09_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Compile `src`; return the built binary's path, or the compiler's stderr.
fn build(tag: &str, src: &str) -> Result<PathBuf, String> {
    let dir = scratch(tag);
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, src).expect("write program source");
    let bin = dir.join("prog");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "-o"])
        .arg(&bin)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    if out.status.success() {
        Ok(bin)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Build `src`, run it, and return its stdout lines.
fn build_run(tag: &str, src: &str) -> Vec<String> {
    let bin = build(tag, src).unwrap_or_else(|e| panic!("the program failed to build:\n{e}"));
    let run = Command::new(&bin).output().expect("run built binary");
    assert!(
        run.status.success(),
        "the built program exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Build `src`, assert it FAILS, and return the compiler's stderr.
fn build_fails(tag: &str, src: &str) -> String {
    match build(tag, src) {
        Ok(_) => panic!("the program was expected to fail to build but succeeded"),
        Err(stderr) => stderr,
    }
}

/// `if COND then A else B` in value position: both arms, in a `return`, in a
/// `let`, nested as an `else if` chain, and with an `int64` destination — the
/// last because the arms take the hint their destination declares, exactly as a
/// bare literal does, and an `i32` constant stored into an `i64` slot is the
/// mistake this form invites.
#[test]
fn a_conditional_value_picks_one_arm() {
    let out = build_run(
        "ternary",
        r#"module ternary

sub label(n: int): text
  return if n = 1 then "item" else "items"
end

sub grade(n: int): text
  return if n >= 90 then "A" else if n >= 80 then "B" else "C"
end

sub bump(): int
  call print_text("ran")
  return 1
end

sub main
  call print_text(label(1))
  call print_text(label(4))
  call print_text(grade(95))
  call print_text(grade(85))
  call print_text(grade(10))
  let zero: int64 = if true then 0 else 99
  let ninety: int64 = if false then 0 else 99
  call print_text(int64_to_text(zero + ninety))
  # The arm not taken is not evaluated: `bump` prints when it runs.
  let n: int = if false then bump() else 5
  call print_text(int_to_text(n))
  call print_text(concat("in an argument: ", if n = 5 then "yes" else "no"))
end
"#,
    );
    assert_eq!(
        out,
        vec![
            "item", "items", "A", "B", "C", "99", "5", "in an argument: yes",
        ],
        "the conditional value picked the wrong arm"
    );
}

/// The two arms are one value, so they must be one type — and the message says
/// which is which rather than blaming the second one alone.
#[test]
fn a_conditional_with_mismatched_arms_is_a_build_error() {
    let stderr = build_fails(
        "ternary_bad",
        "module bad\nsub main\n  let x: int = if true then 1 else \"two\"\nend\n",
    );
    assert!(
        stderr.contains("must have one type")
            && stderr.contains("int")
            && stderr.contains("text"),
        "the diagnostic must name both arm types, got:\n{stderr}"
    );
}

/// A statement that begins with `if` is always the block form; the value form
/// written there gets told where it belongs instead of "expected a newline".
#[test]
fn the_value_form_in_statement_position_names_itself() {
    let stderr = build_fails(
        "ternary_stmt",
        "module bad\nsub main\n  if true then 1 else 2\nend\n",
    );
    assert!(
        stderr.contains("value form"),
        "the diagnostic must point at the value form, got:\n{stderr}"
    );
}

/// `E otherwise F` yields `F` when the call in `E` failed and `E` otherwise —
/// proved against the *same* command reading a file that is there and one that
/// is not, so nothing but the failure differs between the two lines.
#[test]
fn otherwise_falls_back_only_when_the_call_failed() {
    // A directory of its own: `build` clears the scratch dir it compiles in,
    // and this file has to survive that.
    let dir = scratch("otherwise_data");
    let present = dir.join("present.txt");
    std::fs::write(&present, "real contents").expect("write the file that exists");
    let src = format!(
        r#"module fallback
use file

sub main
  let good: text = file_read_text("{p}") otherwise "fallback"
  call print_text(good)
  let bad: text = file_read_text("{p}.missing") otherwise "fallback"
  call print_text(bad)
  # The slot is not cleared: the reason is still readable after the fallback.
  call print_text(if last_error_code() <> 0 then "code kept" else "code lost")
end
"#,
        p = present.display()
    );
    let out = build_run("otherwise", &src);
    assert_eq!(
        out,
        vec!["real contents", "fallback", "code kept"],
        "`otherwise` did not track the error slot"
    );
}

/// One `otherwise` per expression: a second would read a slot the first
/// fallback never cleared, so it would take the last arm every time.
#[test]
fn a_second_otherwise_is_a_build_error() {
    let stderr = build_fails(
        "otherwise_twice",
        "module bad\nuse file\nsub main\n  \
         let x: text = file_read_text(\"a\") otherwise \"b\" otherwise \"c\"\nend\n",
    );
    assert!(
        stderr.contains("one `otherwise`"),
        "the diagnostic must explain the limit, got:\n{stderr}"
    );
}

/// `x.f(a)` IS `f(x, a)` — the same call, spelled left to right. The chain and
/// the nested spelling are compared against each other in one program, so the
/// test cannot pass by both being wrong in the same way.
#[test]
fn a_method_chain_is_the_nested_call() {
    let out = build_run(
        "method",
        r#"module method

sub main
  let s: text = "  ready  "
  call print_text(s.trim().uppercase())
  call print_text(uppercase(trim(s)))
  # A second argument lands after the receiver: `s.find(x)` is `find(s, x)`.
  call print_text(int_to_text("hello".find("ll")))
  call print_text(int_to_text(find("hello", "ll")))
  # The receiver may be any value, not only a name.
  let names: text[] = ["ann", "bo"]
  call print_text(names[2].uppercase())
  # And the statement form, where the result is discarded.
  call s.trim()
end
"#,
    );
    assert_eq!(out[0], out[1], "the chain and the nested call disagree");
    assert_eq!(out[0], "READY");
    assert_eq!(out[2], out[3], "the two-argument method disagrees");
    assert_eq!(out[2], "3", "positions count from 1");
    assert_eq!(out[4], "BO");
}

/// The disambiguation rule: a `.name` followed by `(` is a call, and a `.name`
/// without one is the property read it always was. Built, not run, because a
/// form needs a display and the question here is entirely the parser's.
#[test]
fn a_dotted_name_without_parentheses_is_still_a_property() {
    let src = r#"module props
use ui
form win
  title = "props"
  width = 200
  height = 100
  label greeting
    text = "hello"
    left = 10
    top = 10
    width = 100
    height = 20
  end
end
sub main
  # `greeting.text` reads the property; `.uppercase()` on the result is a call.
  let t: text = greeting.text
  call print_text(t.uppercase())
  call print_text(greeting.text.uppercase())
end
"#;
    if let Err(stderr) = build("props", src) {
        panic!("a property read beside a method call failed to build:\n{stderr}");
    }
}

/// `check` leaves the subroutine when the call it guards failed, and is
/// invisible when it did not. Both paths in one program, and an `int64` sub as
/// well, because the early `return` carries a zero that must take the declared
/// width.
#[test]
fn check_returns_early_and_otherwise_passes_the_value_through() {
    let dir = scratch("check_data");
    let present = dir.join("notes.txt");
    std::fs::write(&present, "notes").expect("write the file that exists");
    let src = format!(
        r#"module propagate
use file

sub load(p: text): text
  let d: text = check file_read_text(p)
  return concat("loaded ", d)
end

sub size(p: text): int64
  let d: text = check file_read_text(p)
  return int_to_int64(length(d))
end

sub copy(from: text, to: text): bool
  let d: text = check file_read_text(from)
  check file_write_text(to, d)
  return true
end

sub main
  call print_text(load("{p}"))
  call print_text(concat("[", concat(load("{p}.missing"), "]")))
  call print_text(int64_to_text(size("{p}")))
  call print_text(int64_to_text(size("{p}.missing")))
  call print_text(if copy("{p}", "{p}.copy") then "copied" else "not copied")
  call print_text(if copy("{p}.missing", "{p}.copy2") then "copied" else "not copied")
  call print_text(file_read_text("{p}.copy"))
end
"#,
        p = present.display()
    );
    let out = build_run("check", &src);
    assert_eq!(
        out,
        vec![
            "loaded notes",
            "[]",
            "5",
            "0",
            "copied",
            "not copied",
            "notes",
        ],
        "`check` did not propagate exactly the failing call"
    );
}

/// A one-line `if` cannot guard a `check`: the suffix would wrap the call and
/// leave the propagation running unconditionally, which is a wrong answer
/// rather than an error. Refused, with the reason.
#[test]
fn a_one_line_if_cannot_guard_a_check() {
    let stderr = build_fails(
        "check_guarded",
        "module bad\nuse file\nsub main\n  check file_write_text(\"a\", \"b\") if true\nend\n",
    );
    assert!(
        stderr.contains("cannot guard a `check`"),
        "the diagnostic must explain why, got:\n{stderr}"
    );
}

/// `check` is a soft keyword: a variable spelled `check` keeps working, because
/// the word is the keyword only when a name follows it.
#[test]
fn check_is_still_usable_as_a_name() {
    let out = build_run(
        "check_name",
        "module soft\nsub main\n  var check: int = 1\n  check = check + 41\n  \
         call print_int(check)\nend\n",
    );
    assert_eq!(out, vec!["42"]);
}

/// A `[]` or `{}` fallback has no type of its own, so it takes the one the
/// value on the left already settled — even where nothing around the
/// expression declares a type.
#[test]
fn an_untyped_fallback_takes_the_value_type() {
    let out = build_run(
        "fallback_empty",
        r#"module empties

sub main
  var d: int{} = {"a": 1, "b": 2}
  call print_int(count(dict_keys(d) otherwise []))
  let xs: text[] = split("a,b,c", ",") otherwise []
  call print_int(count(xs))
end
"#,
    );
    assert_eq!(out, vec!["2", "3"]);
}

/// The callee of a `call through` is a *place*, not a value being built, so the
/// parentheses after it belong to the indirect call and `.name(` there is not
/// the method spelling. A path more than one field deep is where the two would
/// collide.
#[test]
fn a_call_through_callee_is_not_a_method_call() {
    let src = r#"module vtable

record inner is c
  fn: ptr
end

record outer is c
  slot: inner
  fns: ptr[4]
end

sub main
  var o: outer
  call through o.slot.fn(1): int
  call through o.fns[3](2): int
  call print_int(1)
end
"#;
    if let Err(stderr) = build("through", src) {
        panic!("an indirect call through a field path failed to build:\n{stderr}");
    }
}
