//! End-to-end tests for the 0.9.0 control-flow and declaration sugar: `match`
//! with its `when` arms, `repeat N times`, `assert`, `enum`, and the early
//! `return if`.
//!
//! Every one of these is a *rewrite* into something the language already had —
//! an if/else-if chain, a counting loop, a branch around one call, a run of
//! named constants — so the tests prove the OUTPUT. A `match` that type-checks
//! and picks the wrong arm, or evaluates its subject twice, is the failure that
//! matters, and no amount of parser assertion would catch it.
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_sugar09c_{tag}"));
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

/// The LLVM IR for `src`, with `extra` flags. Used to prove that a release
/// build emits *nothing at all* for an `assert` — "compiled out" has to mean
/// no code, and only the IR can say that.
fn emit(tag: &str, src: &str, extra: &[&str]) -> String {
    let dir = scratch(tag);
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, src).expect("write program source");
    let mut args: Vec<&str> = vec!["emit", srcpath.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(&args)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln emit");
    assert!(
        out.status.success(),
        "emit failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// match / when
// ---------------------------------------------------------------------------

/// The whole of `match`: it picks exactly one arm, a `when` may list several
/// values and any of them matches, `else` catches the rest — and the value
/// being tested is evaluated **once**, which the counter proves. A `match`
/// that re-evaluated its subject would run `bump()` three times here and
/// still print the right word, so the count is the assertion that matters.
#[test]
fn a_match_picks_one_arm_and_tests_one_evaluation() {
    let out = build_run(
        "match_once",
        r#"module m
target console

var calls: int = 0

sub bump(): int
  calls = calls + 1
  return 3
end

sub main
  match bump()
  when 1: call print_text("one")
  when 2, 3, 4: call print_text("a few")
  else: call print_text("many")
  end
  call print_int(calls)
end
"#,
    );
    assert_eq!(out, vec!["a few", "1"]);
}

/// An arm may be a block, `else` may be left out, and a `match` that matches
/// nothing does nothing — exactly as an `if` with no `else` does. Text is
/// compared the way text always is.
#[test]
fn a_match_arm_may_be_a_block_and_else_is_optional() {
    let out = build_run(
        "match_block",
        r#"module m
target console

sub main
  var total: int = 0
  match "hello"
  when "hi", "hello":
    total += 10
    total += 5
  when "bye":
    total += 100
  end
  call print_int(total)
  match 99
  when 1: total = 0
  end
  call print_int(total)
end
"#,
    );
    assert_eq!(out, vec!["15", "15"]);
}

/// A `match` is a branch, not a loop: `break` and `continue` inside an arm
/// belong to whatever loop encloses the `match`. Getting this wrong is how a
/// new block statement silently breaks every loop it appears in.
#[test]
fn break_and_continue_inside_an_arm_reach_the_enclosing_loop() {
    let out = build_run(
        "match_loop",
        r#"module m
target console

sub main
  var total: int = 0
  for i in 1..5
    match i
    when 2: continue
    when 4: break
    else: total += i
    end
  end
  call print_int(total)
end
"#,
    );
    // 1 and 3 are added; 2 is skipped, 4 stops the loop, 5 never runs.
    assert_eq!(out, vec!["4"]);
}

/// A `when` compares with `=`, so a value that cannot be compared with the one
/// being tested is a type error — reported in those words, at the `match`,
/// rather than as something about a branch nobody wrote.
#[test]
fn a_when_value_of_the_wrong_type_is_refused() {
    let err = build_fails(
        "match_type",
        r#"module m
target console

sub main
  var n: int = 1
  match n
  when "one": call print_text("no")
  end
end
"#,
    );
    assert!(
        err.contains("int") && err.contains("text"),
        "the message should name both types: {err}"
    );
}

// ---------------------------------------------------------------------------
// repeat N times
// ---------------------------------------------------------------------------

/// `repeat N times` runs the body N times with no visible index, the count is
/// evaluated once, and a count of zero runs nothing. Two of them nest, which
/// is the test that the hidden counters do not collide.
#[test]
fn repeat_runs_the_body_the_count_of_times() {
    let out = build_run(
        "repeat",
        r#"module m
target console

var calls: int = 0

sub three(): int
  calls = calls + 1
  return 3
end

sub main
  repeat 3 times
    call print_text("tick")
  end
  var n: int = 0
  repeat three() times
    repeat 2 times
      n += 1
    end
  end
  call print_int(n)
  call print_int(calls)
  repeat 0 times
    call print_text("never")
  end
  call print_text("end")
end
"#,
    );
    assert_eq!(out, vec!["tick", "tick", "tick", "6", "1", "end"]);
}

// ---------------------------------------------------------------------------
// assert
// ---------------------------------------------------------------------------

/// An `assert` that holds costs the program nothing, and one that fails prints
/// and stops — with a failing exit status, because a script that runs the
/// program has no other way to tell.
#[test]
fn a_failing_assert_prints_and_stops() {
    let src = r#"module m
target console

sub main
  var n: int = 0
  assert n = 0
  call print_text("still here")
  assert n > 0, "n must be positive"
  call print_text("unreachable")
end
"#;
    let bin = build("assert", src).unwrap_or_else(|e| panic!("build failed:\n{e}"));
    let run = Command::new(&bin).output().expect("run built binary");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert!(stdout.contains("still here"), "the passing assert stopped the program: {stdout}");
    assert!(!stdout.contains("unreachable"), "the failing assert did not stop it: {stdout}");
    assert!(stderr.contains("n must be positive"), "the message is missing: {stderr}");
    assert!(!run.status.success(), "a fired assertion must fail the program");
}

/// With no message of its own an `assert` quotes the condition as it was
/// written — the message the author would have typed anyway.
#[test]
fn an_assert_with_no_message_quotes_its_condition() {
    let src = r#"module m
target console

sub main
  var count: int = 2
  assert count = 5
end
"#;
    let bin = build("assert_msg", src).unwrap_or_else(|e| panic!("build failed:\n{e}"));
    let run = Command::new(&bin).output().expect("run built binary");
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert!(
        stderr.contains("count = 5"),
        "the auto-message should quote the condition: {stderr}"
    );
}

/// A release build compiles asserts OUT: not a branch that is never taken, not
/// a message left in the binary — no code at all. The IR is the only place
/// that can be checked.
#[test]
fn a_release_build_emits_no_assert_at_all() {
    let src = r#"module m
target console

sub main
  var n: int = 0
  assert n > 0, "the release build must not carry this"
  call print_text("done")
end
"#;
    let debug = emit("assert_dbg", src, &[]);
    assert!(
        debug.contains("assert_failed"),
        "a debug build keeps the check"
    );
    let release = emit("assert_rel", src, &["--release"]);
    assert!(
        !release.contains("assert_failed"),
        "a release build must not call the failure handler"
    );
    assert!(
        !release.contains("the release build must not carry this"),
        "a release build must not carry the message either"
    );
    assert!(release.contains("done"), "the rest of the program is untouched");
}

/// Dropping the code is not the same as not looking at it: a release build has
/// to refuse every mistake a debug build refuses, so an `assert` whose
/// condition is nonsense is a compile error in both.
#[test]
fn a_release_build_still_checks_what_it_drops() {
    let dir = scratch("assert_check");
    let srcpath = dir.join("prog.kiln");
    std::fs::write(
        &srcpath,
        r#"module m
target console

sub main
  assert nosuchthing > 0
  call print_text("hi")
end
"#,
    )
    .expect("write program source");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["emit", srcpath.to_str().unwrap(), "--release"])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln emit");
    assert!(!out.status.success(), "a release build accepted a broken assert");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("nosuchthing"),
        "the message should name it: {err}"
    );
}

// ---------------------------------------------------------------------------
// enum
// ---------------------------------------------------------------------------

/// An enum's members are ints numbered from **1**, in declaration order — the
/// same place every position in Kiln counts from. They are reached only
/// through the enum's name, so two enums may both have a `red`, and the name
/// written as a type is `int`: a parameter typed by the enum takes its members
/// and takes a plain int, because that is what they are.
#[test]
fn enum_members_count_from_one_and_type_a_parameter() {
    let out = build_run(
        "enum",
        r#"module m
target console

enum colour
  red, green
  blue
end

enum size
  red
  large
end

sub name_of(c: colour): text
  match c
  when colour.red: return "red"
  when colour.green: return "green"
  else: return "blue"
  end
end

sub main
  call print_int(colour.red)
  call print_int(colour.green)
  call print_int(colour.blue)
  call print_int(size.red)
  call print_int(size.large)
  call print_text(name_of(colour.blue))
  call print_text(name_of(2))
  call print_text("a {colour.green} sky")
end
"#,
    );
    assert_eq!(
        out,
        vec!["1", "2", "3", "1", "2", "blue", "green", "a 2 sky"]
    );
}

/// A member that does not exist is named where it is written, with the ones
/// that do listed — not left to surface downstream as an unknown component.
#[test]
fn a_misspelt_enum_member_is_refused_by_name() {
    let err = build_fails(
        "enum_bad",
        r#"module m
target console

enum colour
  red, green
end

sub main
  call print_int(colour.purple)
end
"#,
    );
    assert!(
        err.contains("purple") && err.contains("colour"),
        "the message should name both: {err}"
    );
}

// ---------------------------------------------------------------------------
// return if
// ---------------------------------------------------------------------------

/// `return if COND` is the early-exit guard, and `return if not COND` its
/// inverse. Both are the one-line `if` suffix over a `return`, and the
/// conditional *value* `return if c then a else b` still means the value —
/// `then` on the line is what tells the two apart.
#[test]
fn return_if_is_a_guard_and_still_leaves_the_value_form_alone() {
    let out = build_run(
        "return_if",
        r#"module m
target console

sub valid(n: int): bool
  return n > 0
end

sub describe(n: int)
  return if not valid(n)
  call print_text("valid {n}")
end

sub label(n: int): text
  return if n = 1 then "item" else "items"
end

sub main
  call describe(5)
  call describe(-1)
  call print_text(label(1))
  call print_text(label(3))
end
"#,
    );
    assert_eq!(out, vec!["valid 5", "item", "items"]);
}

// ---------------------------------------------------------------------------
// the words stay names
// ---------------------------------------------------------------------------

/// Every word this stage adds is a SOFT keyword: it leads a statement only
/// when what follows could not continue an assignment to a variable of that
/// name. A program that already has a variable called `match`, `repeat`,
/// `assert`, `when`, `times` or `enum` keeps compiling, and the core command
/// `repeat(text, n)` is untouched.
#[test]
fn the_new_words_are_still_ordinary_names() {
    let out = build_run(
        "soft",
        r#"module m
target console

sub main
  var match: int = 1
  var repeat: int = 2
  var assert: int = 3
  var when: int = 4
  var times: int = 5
  var enum: int = 6
  match = match + 1
  repeat += 1
  assert *= 2
  when mod= 3
  times /= 5
  enum -= 1
  call print_int(match)
  call print_int(repeat)
  call print_int(assert)
  call print_int(when)
  call print_int(times)
  call print_int(enum)
  call print_text(repeat("ab", 2))
end
"#,
    );
    assert_eq!(out, vec!["2", "3", "6", "1", "1", "5", "abab"]);
}
