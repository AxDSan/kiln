//! End-to-end tests for the 0.8.0 iteration sugar: range loops (`for i in
//! A..B`) and `for each`. Both desugar to the counting `for` the language
//! already lowers — 1-based, inclusive — so these build real programs, run
//! them, and prove the OUTPUT, because sugar that type-checks but iterates the
//! wrong count is the failure that matters. A handful of build-error cases pin
//! the rules that must stay strict: only a collection is iterable, two bindings
//! are a dictionary's, and the bindings are immutable.
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build `src` to a temp binary, run it, and return its stdout lines. `tag` must
/// be unique per test so parallel runs never share an output path.
fn build_run(tag: &str, src: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("kiln_iter_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, src).expect("write program source");
    let bin = dir.join("prog");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "-o"])
        .arg(&bin)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        out.status.success(),
        "the program failed to build:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let dir = std::env::temp_dir().join(format!("kiln_iter_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let srcpath = dir.join("prog.kiln");
    std::fs::write(&srcpath, src).expect("write program source");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "-o"])
        .arg(dir.join("prog"))
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        !out.status.success(),
        "the program was expected to fail to build but succeeded"
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A range loop counts up inclusively — 1..10 is ten turns, summing to 55 — and
/// the loop variable is the value, not an index.
#[test]
fn range_up_sum() {
    let out = build_run(
        "range_up",
        r#"module iter
sub main
  var sum: int = 0
  for i in 1..10
    sum += i
  end
  call print_int(sum)
end
"#,
    );
    assert_eq!(out, ["55"]);
}

/// `step -1` counts DOWN, and inclusively: 10..1 yields 10, 9, … 1 in order.
#[test]
fn range_down_step() {
    let out = build_run(
        "range_down",
        r#"module iter
sub main
  for i in 10..1 step -1
    call print_int(i)
  end
end
"#,
    );
    assert_eq!(out, ["10", "9", "8", "7", "6", "5", "4", "3", "2", "1"]);
}

/// A positive `step` larger than one lands on the inclusive endpoint when it is
/// reachable and stops before overshooting it when it is not.
#[test]
fn range_step_three() {
    let out = build_run(
        "range_step3",
        r#"module iter
sub main
  for i in 1..10 step 3
    call print_int(i)
  end
end
"#,
    );
    // 1, 4, 7, 10 — the next would be 13, past the limit.
    assert_eq!(out, ["1", "4", "7", "10"]);
}

/// A range's bounds are evaluated ONCE, before the loop: the limit is a call
/// that bumps a global, and the global ends at 1 no matter how many turns run.
#[test]
fn range_bounds_evaluated_once() {
    let out = build_run(
        "range_once",
        r#"module iter
var calls: int = 0

sub hi(): int
  increment calls
  return 3
end

sub main
  var sum: int = 0
  for i in 1..hi()
    sum += i
  end
  call print_int(sum)
  call print_int(calls)
end
"#,
    );
    // 1+2+3 = 6, and the limit call ran exactly once.
    assert_eq!(out, ["6", "1"]);
}

/// `for each` over an int array binds each element in turn.
#[test]
fn each_array_sum() {
    let out = build_run(
        "each_array",
        r#"module iter
sub main
  var xs: int[] = [10, 20, 30, 40]
  var sum: int = 0
  for each x in xs
    sum += x
  end
  call print_int(sum)
end
"#,
    );
    assert_eq!(out, ["100"]);
}

/// `for each k, v in d` binds both the key and the value each turn. A
/// dictionary has no guaranteed order, so the test sorts the emitted lines and
/// compares against the expected SET — both bindings must be seen for every
/// entry.
#[test]
fn each_dict_key_value() {
    let mut out = build_run(
        "each_dict",
        r#"module iter
sub main
  var d: int{} = {"a": 1, "b": 2, "c": 3}
  for each k, v in d
    call print_text("{k}={v}")
  end
end
"#,
    );
    out.sort();
    assert_eq!(out, ["a=1", "b=2", "c=3"]);
}

/// The single-binding `for each k in d` binds the key only.
#[test]
fn each_dict_key_only() {
    let mut out = build_run(
        "each_dict_key",
        r#"module iter
sub main
  var d: int{} = {"x": 10, "y": 20}
  for each k in d
    call print_text(k)
  end
end
"#,
    );
    out.sort();
    assert_eq!(out, ["x", "y"]);
}

/// `for each` over a byte-set reads each byte as an `int`, 1-based.
#[test]
fn each_bytes() {
    let out = build_run(
        "each_bytes",
        r#"module iter
sub main
  var bs: bytes = bytes_new(3)
  call bytes_set(bs, 1, 65)
  call bytes_set(bs, 2, 66)
  call bytes_set(bs, 3, 67)
  var sum: int = 0
  for each b in bs
    sum += b
  end
  call print_int(sum)
end
"#,
    );
    // 65 + 66 + 67 = 198.
    assert_eq!(out, ["198"]);
}

/// `for each` over a text yields each character as a one-character text.
#[test]
fn each_text_chars() {
    let out = build_run(
        "each_text",
        r#"module iter
sub main
  var s: text = "hi!"
  for each ch in s
    call print_text(ch)
  end
end
"#,
    );
    assert_eq!(out, ["h", "i", "!"]);
}

/// `for each x at i` binds the 1-based index alongside the element.
#[test]
fn each_at_index() {
    let out = build_run(
        "each_at",
        r#"module iter
sub main
  var xs: text[] = ["a", "b", "c"]
  for each x at i in xs
    call print_text("{i}:{x}")
  end
end
"#,
    );
    assert_eq!(out, ["1:a", "2:b", "3:c"]);
}

/// `break` and `continue` work inside a `for each`: `continue` skips a turn,
/// `break` ends the loop, and the counter still advances correctly across a
/// `continue` (the classic while-desugar bug this avoids by lowering to `for`).
#[test]
fn each_break_continue() {
    let out = build_run(
        "each_bc",
        r#"module iter
sub main
  var xs: int[] = [1, 2, 3, 4, 5, 6]
  var sum: int = 0
  for each x in xs
    if x = 2
      continue
    end
    if x = 5
      break
    end
    sum += x
  end
  call print_int(sum)
end
"#,
    );
    // 1 (+1), 2 skipped, 3 (+3), 4 (+4), 5 breaks: 1+3+4 = 8.
    assert_eq!(out, ["8"]);
}

/// Interpolation and iteration together, the headline ergonomic combination:
/// `for each x in xs` with `print_text("item {x}")`.
#[test]
fn each_with_interpolation() {
    let out = build_run(
        "each_interp",
        r#"module iter
sub main
  var xs: int[] = [7, 8, 9]
  for each x in xs
    call print_text("item {x}")
  end
end
"#,
    );
    assert_eq!(out, ["item 7", "item 8", "item 9"]);
}

/// A nested `for each` inside a range loop: the two counters do not collide and
/// each element is visited the right number of times.
#[test]
fn nested_range_and_each() {
    let out = build_run(
        "nested",
        r#"module iter
sub main
  var xs: int[] = [1, 2]
  var total: int = 0
  for r in 1..3
    for each x in xs
      total += x
    end
  end
  call print_int(total)
end
"#,
    );
    // (1+2) summed three times = 9.
    assert_eq!(out, ["9"]);
}

// --- The rules that must stay strict ---------------------------------------

/// A `for each` iterates a collection; a scalar is refused with a clear message.
#[test]
fn each_non_collection_rejected() {
    let msg = build_fails(
        "each_scalar",
        r#"module iter
sub main
  var n: int = 5
  for each x in n
    call print_int(x)
  end
end
"#,
    );
    assert!(
        msg.contains("for each") && msg.contains("array"),
        "expected a message naming what `for each` can iterate, got:\n{msg}"
    );
}

/// Two bindings (`k, v`) read a dictionary; over an array it is a mistake, said
/// so rather than silently binding a phantom value.
#[test]
fn each_two_bindings_on_array_rejected() {
    let msg = build_fails(
        "each_two_array",
        r#"module iter
sub main
  var xs: int[] = [1, 2, 3]
  for each k, v in xs
    call print_int(v)
  end
end
"#,
    );
    assert!(
        msg.contains("two bindings") || msg.contains("dictionary"),
        "expected a message about two bindings needing a dictionary, got:\n{msg}"
    );
}

/// The element binding is immutable, like a `for` counter: assigning to it is an
/// error, not a silent no-op.
#[test]
fn each_element_is_immutable() {
    let msg = build_fails(
        "each_immut",
        r#"module iter
sub main
  var xs: int[] = [1, 2, 3]
  for each x in xs
    x = 99
  end
end
"#,
    );
    assert!(
        msg.contains("cannot be assigned") || msg.contains("immutable"),
        "expected a message that the loop binding cannot be assigned, got:\n{msg}"
    );
}
