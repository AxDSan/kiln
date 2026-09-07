//! End-to-end tests for the 0.9.0 subroutine ergonomics: an unannotated `let`,
//! parameter defaults, named arguments, and the record literal with its update
//! form.
//!
//! All four are rewrites — an inferred `let` becomes the typed one, a named
//! argument becomes a positional one, an omitted argument becomes the default
//! expression written at the call, `p{..q, y: 9}` becomes the literal with
//! every field spelled out — so what these tests prove is the OUTPUT. A named
//! argument that type-checks and lands in the wrong slot is the failure that
//! matters, and no parser assertion would catch it.
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_sugar09s_{tag}"));
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

/// `src` must fail to build, with a message containing `needle`.
fn refuses(tag: &str, src: &str, needle: &str) {
    match build(tag, src) {
        Ok(_) => panic!("this should not have built:\n{src}"),
        Err(msg) => assert!(
            msg.contains(needle),
            "expected a message containing {needle:?}, got:\n{msg}"
        ),
    }
}

// --- 1. `let` / `var` without the annotation --------------------------------

#[test]
fn an_unannotated_let_takes_its_type_from_the_value() {
    let out = build_run(
        "infer",
        r#"module m
sub twice(n: int): int
  return n * 2
end
sub main
  let xs: text[] = ["a", "b", "c"]
  let n = count(xs)
  let name = concat("wor", "ld")
  let ok = n > 2
  let big = twice(n)
  var total = 0
  total += big
  let d = 1.5 + 0.25
  call print_int(n)
  call print_text(name)
  call print_int(if ok then 1 else 0)
  call print_int(total)
  call print_double(d)
end
"#,
    );
    assert_eq!(out, ["3", "world", "1", "6", "1.75"]);
}

#[test]
fn an_inferred_binding_is_still_typed() {
    // `n` is an `int`, and using it as a `text` is the same mistake it would be
    // if the type had been written out.
    refuses(
        "infer_typed",
        "module m\nsub main\n  let n = 1 + 2\n  call print_text(n)\nend\n",
        "expects text",
    );
}

#[test]
fn a_value_that_says_nothing_about_its_type_is_refused() {
    refuses(
        "infer_empty",
        "module m\nsub main\n  let xs = []\nend\n",
        "cannot be read off its value",
    );
    refuses(
        "infer_nothing",
        "module m\nsub main\n  var q\nend\n",
        "neither a type nor a value",
    );
}

// --- 2. defaults ------------------------------------------------------------

const CONNECT: &str = r#"module m
sub connect(host: text, port: int = 80, timeout: int = 5000): text
  return "{host}:{port} in {timeout}"
end
"#;

#[test]
fn trailing_defaults_fill_the_arguments_a_call_leaves_out() {
    let out = build_run(
        "defaults",
        &format!(
            "{CONNECT}sub main
  call print_text(connect(\"a\"))
  call print_text(connect(\"b\", 8080))
  call print_text(connect(\"c\", 8080, 1))
end
"
        ),
    );
    assert_eq!(
        out,
        ["a:80 in 5000", "b:8080 in 5000", "c:8080 in 1"]
    );
}

#[test]
fn a_default_is_the_expression_evaluated_at_the_call() {
    // The default is a call of its own, so this also proves it is *run*, not
    // folded to a constant at the declaration.
    let out = build_run(
        "default_expr",
        r#"module m
sub greet(who: text, greeting: text = concat("hel", "lo")): text
  return "{greeting}, {who}"
end
sub main
  call print_text(greet("world"))
  call print_text(greet("world", "hi"))
end
"#,
    );
    assert_eq!(out, ["hello, world", "hi, world"]);
}

#[test]
fn a_default_may_itself_be_written_with_sugar() {
    // The default is copied to the call, so it has to be rewritten there too —
    // a named argument or a record literal inside one is not a special case.
    let out = build_run(
        "default_sugar",
        r#"module m
record point
  x: int
  y: int
end
sub at(a: int, b: int): int
  return a * 10 + b
end
sub code(v: int = at(b: 2, a: 1)): int
  return v
end
sub area(p: point = point{x: 3, y: 4}): int
  return p.x * p.y
end
sub main
  call print_int(code())
  call print_int(code(7))
  call print_int(area())
  call print_int(area(point{x: 5, y: 5}))
end
"#,
    );
    assert_eq!(out, ["12", "7", "12", "25"]);
}

#[test]
fn a_default_that_fills_itself_in_is_refused() {
    refuses(
        "default_cycle",
        "module m
sub f(a: int = f()): int
  return a
end
sub main
  call print_int(f())
end
",
        "never finishes",
    );
}

#[test]
fn a_default_before_a_required_parameter_is_refused() {
    refuses(
        "gap",
        "module m\nsub f(a: int = 1, b: int)\nend\nsub main\nend\n",
        "only allowed on the last parameters",
    );
}

#[test]
fn an_argument_with_no_default_must_be_given() {
    refuses(
        "missing",
        &format!("{CONNECT}sub main\n  call print_text(connect())\nend\n"),
        "was not given `host`",
    );
}

#[test]
fn a_default_cannot_read_a_parameter() {
    // It is evaluated where the call is written, and `a` means nothing there.
    refuses(
        "default_reads_param",
        "module m\nsub f(a: int, b: int = a)\nend\nsub main\n  call f(1)\nend\n",
        "cannot use anything from here",
    );
}

#[test]
fn a_foreign_function_cannot_declare_a_default() {
    refuses(
        "dll_default",
        "module m\ndll g(a: int = 1) from \"c\"\nsub main\nend\n",
        "cannot have a default",
    );
}

// --- 3. named arguments -----------------------------------------------------

#[test]
fn named_arguments_go_to_the_slot_they_name_in_any_order() {
    let out = build_run(
        "named",
        &format!(
            "{CONNECT}sub main
  call print_text(connect(host: \"a\", port: 1, timeout: 2))
  call print_text(connect(timeout: 2, host: \"b\", port: 1))
  call print_text(connect(host: \"c\", timeout: 9))
  call print_text(connect(\"d\", timeout: 7))
  call print_text(connect(\"e\", 1, timeout: 7))
end
"
        ),
    );
    assert_eq!(
        out,
        [
            "a:1 in 2",
            "b:1 in 2",
            "c:80 in 9",
            "d:80 in 7",
            "e:1 in 7",
        ]
    );
}

#[test]
fn a_named_argument_works_in_expression_and_statement_position() {
    let out = build_run(
        "named_positions",
        r#"module m
sub add(a: int, b: int): int
  return a + b
end
sub show(label: text, value: int)
  call print_text("{label}={value}")
end
sub main
  let n = add(b: 2, a: 40)
  call show(value: n, label: "n")
  call show("m", value: add(a: 1, b: 1))
end
"#,
    );
    assert_eq!(out, ["n=42", "m=2"]);
}

#[test]
fn a_name_that_is_not_a_parameter_is_refused() {
    refuses(
        "unknown_name",
        &format!("{CONNECT}sub main\n  call print_text(connect(host: \"a\", prot: 1))\nend\n"),
        "has no parameter `prot`",
    );
}

#[test]
fn the_same_parameter_cannot_be_given_twice() {
    refuses(
        "twice",
        &format!("{CONNECT}sub main\n  call print_text(connect(\"a\", host: \"b\"))\nend\n"),
        "given `host` twice",
    );
}

#[test]
fn a_positional_argument_may_not_follow_a_named_one() {
    refuses(
        "order",
        &format!("{CONNECT}sub main\n  call print_text(connect(host: \"a\", 1))\nend\n"),
        "named arguments come last",
    );
}

#[test]
fn a_foreign_function_takes_named_arguments_too() {
    // A `dll` declares parameter names exactly as a `sub` does, so the win kit
    // gets this for free. The slot mapping is proven by the types: swap the two
    // names over and the same call stops type-checking.
    let ok = "module m\ndll pick(count: int, label: text): int from \"demo\"\n\
              sub main\n  call print_int(pick(label: \"a\", count: 1))\nend\n";
    build("dll_named", ok).expect("a named argument should reach the parameter of that name");
    refuses(
        "dll_named_swapped",
        "module m\ndll pick(count: int, label: text): int from \"demo\"\n\
         sub main\n  call print_int(pick(label: 1, count: \"a\"))\nend\n",
        "argument 1 expects int",
    );
}

// --- 4. record literals and updates -----------------------------------------

#[test]
fn a_record_literal_reads_back_field_by_field() {
    let out = build_run(
        "record_lit",
        r#"module m
record point
  x: int
  y: int
  label: text
end
sub main
  let p = point{x: 3, y: 4, label: "p"}
  let q = point{
    x: 10,
    y: 20,
    label: "q",
  }
  call print_text("{p.label} {p.x},{p.y}")
  call print_text("{q.label} {q.x},{q.y}")
end
"#,
    );
    assert_eq!(out, ["p 3,4", "q 10,20"]);
}

#[test]
fn an_update_literal_copies_the_fields_it_does_not_name() {
    let out = build_run(
        "record_update",
        r#"module m
record point
  x: int
  y: int
  label: text
end
sub main
  let p = point{x: 3, y: 4, label: "p"}
  let q = point{...p, y: 9}
  let r = point{..q, label: "r"}
  call print_text("{p.x},{p.y},{p.label}")
  call print_text("{q.x},{q.y},{q.label}")
  call print_text("{r.x},{r.y},{r.label}")
end
"#,
    );
    // The original is untouched: an update is a new record, not a write.
    assert_eq!(out, ["3,4,p", "3,9,p", "3,9,r"]);
}

#[test]
fn the_brace_and_paren_spellings_are_the_same_record() {
    let out = build_run(
        "record_spellings",
        r#"module m
record point
  x: int
  y: int
end
sub sum(p: point): int
  return p.x + p.y
end
sub main
  call print_int(sum(point(x: 1, y: 2)))
  call print_int(sum(point{x: 1, y: 2}))
  call print_int(sum(p: point{x: 10, y: 20}))
end
"#,
    );
    assert_eq!(out, ["3", "3", "30"]);
}

#[test]
fn an_update_names_a_field_the_record_has() {
    refuses(
        "update_unknown",
        "module m\nrecord point\n  x: int\nend\nsub main\n  let p = point{x: 1}\n  \
         let q = point{...p, z: 2}\nend\n",
        "has no field `z`",
    );
}

#[test]
fn an_update_copies_from_a_name_rather_than_from_work() {
    refuses(
        "update_call",
        "module m\nrecord point\n  x: int\n  y: int\nend\nsub make(): point\n  \
         return point(x: 1, y: 2)\nend\nsub main\n  let q = point{...make(), y: 3}\nend\n",
        "name a variable there",
    );
}

// --- c-records --------------------------------------------------------------

#[test]
fn a_c_record_literal_is_the_zeroed_declaration_plus_its_field_writes() {
    let out = build_run(
        "c_record_lit",
        r#"module m
record R is c
  x: int
  y: int
  z: int
end
sub main
  var r: R = R{x: 3, z: 5}
  let s = R{y: 7}
  call print_int(r.x)
  call print_int(r.y)
  call print_int(r.z)
  call print_int(s.y)
  call print_int64(size of R)
end
"#,
    );
    // Every field the literal left out keeps the zero the declaration gave it.
    assert_eq!(out, ["3", "0", "5", "7", "12"]);
}
