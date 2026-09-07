//! End-to-end tests for the 0.9.0 hard tier: the optional type `T?`, the list
//! built by a loop, and `defer`.
//!
//! All three are rewrites into what the language already had — a value with a
//! hidden truth beside it, a `for each` with an `append` in it, the cleanup
//! statement written out at each exit — so the tests prove the OUTPUT, and the
//! output is a *sequence*. A `defer` that runs, but after the value it was
//! meant to run behind, and an optional that unwraps to a stale error slot both
//! type-check perfectly; only the order the program prints in catches them.
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_sugar09hard_{tag}"));
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

// --- optionals ----------------------------------------------------------

/// A `T?` is unwrapped two ways, and both leave a plain `T` behind: `otherwise`
/// supplies the value that is not there, `if some ... as` binds the one that is.
/// The lookup that misses and the lookup that hits go down the same two paths,
/// because a fallback that is only ever exercised by the failing case is a
/// fallback nobody has proved returns the *right* value in the other one.
#[test]
fn an_optional_unwraps_with_otherwise_and_with_if_some() {
    let out = build_run(
        "unwrap",
        r#"module opt

sub main
  let ages: int{} = {"ann": 30}
  let hit: int? = dict_get(ages, "ann")
  let miss: int? = dict_get(ages, "zed")
  call print_int(hit otherwise -1)
  call print_int(miss otherwise -1)
  if some hit as found
    call print_text("found {found}")
  else
    call print_text("hit was empty")
  end
  if some miss as absent
    call print_text("found {absent}")
  else
    call print_text("miss was empty")
  end
end
"#,
    );
    assert_eq!(out, ["30", "-1", "found 30", "miss was empty"]);
}

/// `none` is the empty optional, and a `var` may be filled afterwards: the two
/// halves are rewritten together, so an assignment says "the value is there"
/// exactly as a declaration does.
#[test]
fn none_is_the_empty_optional_and_a_var_can_be_filled_later() {
    let out = build_run(
        "none",
        r#"module opt

sub main
  var slot: text? = none
  call print_text(slot otherwise "(empty)")
  slot = "filled"
  call print_text(slot otherwise "(empty)")
  if some slot as v
    call print_text("held {v}")
  end
end
"#,
    );
    assert_eq!(out, ["(empty)", "filled", "held filled"]);
}

/// The whole point of the type: a `T?` is not a `T`, and the checker says so
/// before the program can read a value that is not there.
#[test]
fn using_an_optional_raw_is_refused() {
    let err = build_fails(
        "raw",
        r#"module opt

sub main
  let ages: int{} = {"ann": 30}
  let a: int? = dict_get(ages, "ann")
  call print_int(a)
end
"#,
    );
    assert!(
        err.contains("expects int, got int?"),
        "the checker should refuse an optional where the value is wanted:\n{err}"
    );
    assert!(
        err.contains("otherwise") && err.contains("if some"),
        "the refusal should say how to unwrap it:\n{err}"
    );
}

/// An optional is a local's type and only a local's: it is a value plus a
/// hidden truth beside it, which is not a shape the slot ABI has. A parameter
/// is refused where it is written, by the parser, rather than three passes
/// later by something that had to guess what was meant.
#[test]
fn an_optional_cannot_be_a_parameter() {
    let err = build_fails(
        "param",
        r#"module opt

sub takes(v: text?)
  call print_text(v otherwise "x")
end

sub main
  call takes("hi")
end
"#,
    );
    assert!(
        err.contains("only a local"),
        "an optional parameter should be refused as such:\n{err}"
    );
}

/// The truth beside the value is *this* initializer's verdict, not whatever
/// failure the program last suffered. A command that cannot fail never touches
/// the error slot, so without clearing it first the `length` below would
/// inherit the missing key's failure and read as absent.
#[test]
fn an_optional_does_not_inherit_an_earlier_failure() {
    let out = build_run(
        "stale",
        r#"module opt

sub main
  let ages: int{} = {"ann": 30}
  let miss: int? = dict_get(ages, "zed")
  call print_int(miss otherwise -1)
  let n: int? = length("hello")
  call print_int(n otherwise -99)
end
"#,
    );
    assert_eq!(out, ["-1", "5"]);
}

// --- list comprehensions ------------------------------------------------

/// `[EXPR for each x in xs]` builds what the loop would have built, with and
/// without a `where`. The `where` case is the one that proves the filter runs
/// per element rather than over the finished list.
#[test]
fn a_list_can_be_built_by_a_loop_with_and_without_where() {
    let out = build_run(
        "comp",
        r#"module comp

sub main
  let xs: int[] = [1, 2, 3, 4, 5]
  let doubled: int[] = [n * 2 for each n in xs]
  call print_text(join([int_to_text(d) for each d in doubled], ","))
  let evens: int[] = [n for each n in xs where mod_int(n, 2) = 0]
  call print_text(join([int_to_text(e) for each e in evens], ","))
  let names: text[] = ["<{w}>" for each w in ["a", "b"]]
  call print_text(join(names, ""))
  let empty: int[] = [n for each n in xs where n > 99]
  call print_int(count(empty))
end
"#,
    );
    assert_eq!(out, ["2,4,6,8,10", "2,4", "<a><b>", "0"]);
}

/// The header is the statement `for each`'s, word for word: the `at IDX`
/// position counts from 1, and a dictionary binds its key and its value.
#[test]
fn a_comprehension_takes_the_whole_for_each_header() {
    let out = build_run(
        "compheader",
        r#"module comp

sub main
  let marked: text[] = ["{i}{c}" for each c at i in "abc"]
  call print_text(join(marked, " "))
  let ages: int{} = {"ann": 30}
  let lines: text[] = ["{k}={v}" for each k, v in ages]
  call print_text(join(lines, " "))
end
"#,
    );
    assert_eq!(out, ["1a 2b 3c", "ann=30"]);
}

/// A comprehension's bindings are locals of the enclosing subroutine, like a
/// loop's, so one may not take a name that already means something. Without
/// this the outer name would quietly become the loop's last element for the
/// rest of the subroutine — the collection is walked in the same flat scope
/// every other binding lives in.
#[test]
fn a_comprehension_binding_cannot_shadow_a_local() {
    let err = build_fails(
        "compshadow",
        r#"module comp

sub main
  let n: int = 5
  let xs: int[] = [1, 2, 3]
  let ys: int[] = [n * 2 for each n in xs]
  call print_int(n)
  call print_int(count(ys))
end
"#,
    );
    assert!(
        err.contains("`n` is defined more than once"),
        "a comprehension may not reuse a name in scope:
{err}"
    );
}

/// Two comprehensions in one subroutine may each bind `n`, though: neither
/// binding outlives its own brackets, so there is nothing for the second to
/// collide with.
#[test]
fn two_comprehensions_can_share_a_binding_name() {
    let out = build_run(
        "comptwice",
        r#"module comp

sub main
  let xs: int[] = [1, 2, 3]
  let a: int[] = [n * 2 for each n in xs]
  let b: int[] = [n * 3 for each n in xs]
  call print_int(a[3])
  call print_int(b[3])
end
"#,
    );
    assert_eq!(out, ["6", "9"]);
}

// --- defer --------------------------------------------------------------

/// The cleanup runs on the early `return` and on falling off the end, in
/// reverse order of declaration — and, on the return, *after* the value has
/// been computed. The last is what the pattern exists for: a `defer` that
/// closed the handle before the `return` read it would print in the same order
/// and answer wrong.
#[test]
fn defer_runs_on_an_early_return_and_on_fall_through() {
    let out = build_run(
        "defer",
        r#"module def

var log: text = ""

sub note(what: text)
  log = log + what + ";"
end

sub pick(n: int): int
  call note("open A")
  defer call note("close A")
  call note("open B")
  defer call note("close B")
  return 10 if n = 1
  call note("body")
  return 20
end

sub main
  call print_int(pick(1))
  call print_text(log)
  log = ""
  call print_int(pick(2))
  call print_text(log)
end
"#,
    );
    assert_eq!(
        out,
        [
            "10",
            "open A;open B;close B;close A;",
            "20",
            "open A;open B;body;close B;close A;",
        ]
    );
}

/// The value a `return` carries is computed before the cleanup runs. Written
/// with an assignment rather than a file handle so the test needs no
/// filesystem, but it is the same hazard: the cleanup changes what the return
/// expression reads.
#[test]
fn the_returned_value_is_taken_before_the_cleanup_runs() {
    let out = build_run(
        "deferorder",
        r#"module def

var n: int = 1

sub answer(): int
  defer n = 99
  return n
end

sub main
  call print_int(answer())
  call print_int(n)
end
"#,
    );
    assert_eq!(out, ["1", "99"]);
}

/// A `defer` belongs to the block it was written in, so one in a loop body runs
/// every turn — including the turn a `continue` cuts short, which is a way out
/// of that block like any other.
#[test]
fn defer_in_a_loop_body_runs_every_turn() {
    let out = build_run(
        "deferloop",
        r#"module def

sub main
  var i: int = 0
  while i < 4
    i += 1
    defer call print_text("end {i}")
    if i = 2
      continue
    end
    if i = 3
      break
    end
    call print_text("work {i}")
  end
  call print_text("after")
end
"#,
    );
    assert_eq!(
        out,
        ["work 1", "end 1", "end 2", "end 3", "after"]
    );
}

/// `defer` runs one simple statement. A block would have an end of its own, and
/// "the end of the block" is the whole of what a `defer` means.
#[test]
fn defer_refuses_a_block() {
    let err = build_fails(
        "deferblock",
        r#"module def

sub main
  defer if 1 = 1
    call print_text("hi")
  end
  call print_text("body")
end
"#,
    );
    assert!(
        err.contains("one simple statement"),
        "a deferred block should be refused as such:\n{err}"
    );
}
