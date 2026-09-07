//! End-to-end tests for the 0.8.0 assignment and operator shorthands: build a
//! program that exercises every desugar, run it, and prove the OUTPUT — because
//! sugar that type-checks but computes the wrong thing is the failure that
//! matters. Two build-error cases prove the type rules that must stay strict:
//! `+` does not auto-stringify, and `in` refuses a type with no members.
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
    let dir = std::env::temp_dir().join(format!("kiln_sugar_{tag}"));
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
    let dir = std::env::temp_dir().join(format!("kiln_sugar_{tag}"));
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

/// Every desugar in one program: compound assignment (`+= -= *= /= mod= &=`),
/// `increment`/`decrement`, text `+` and `*`, a true and a false chained
/// comparison, the middle evaluated once, `in`/`not in` over an array, a
/// dictionary and text, a one-line `if` that fires and one that does not,
/// a decimal underscore literal, and trailing commas.
#[test]
fn every_shorthand_runs_correctly() {
    let src = "\
module sugar
var calls: int = 0

sub bump(): int
  increment calls
  return 5
end

sub main
  var x: int = 10
  x += 5
  x -= 3
  x *= 2
  x /= 4
  x mod= 4
  call print_int(x)

  var s: text = \"a\"
  s &= \"b\"
  s &= \"c\"
  call print_text(s)

  var n: int = 0
  increment n
  increment n
  decrement n
  call print_int(n)

  call print_text(\"foo\" + \"bar\")
  call print_text(\"ab\" * 3)

  var v: int = 5
  if 1 <= v <= 12
    call print_text(\"t1\")
  end
  if 1 <= 20 <= 12
    call print_text(\"nope\")
  else
    call print_text(\"f1\")
  end

  if 1 <= bump() <= 10
    call print_text(\"mid-in\")
  end
  call print_int(calls)

  var xs: int[] = [10, 20, 30]
  if 20 in xs
    call print_text(\"has20\")
  end
  if 99 not in xs
    call print_text(\"no99\")
  end

  var d: int{} = {\"a\": 1, \"b\": 2}
  if \"a\" in d
    call print_text(\"hasA\")
  end
  if \"z\" not in d
    call print_text(\"noZ\")
  end

  if \"ell\" in \"hello\"
    call print_text(\"substr\")
  end
  if \"xyz\" not in \"hello\"
    call print_text(\"nosub\")
  end

  var y: int = 3
  call print_text(\"fires\") if y = 3
  call print_text(\"skip\") if y = 99
  increment y if y = 3
  call print_int(y)

  call print_int(1_000_000)

  var zs: int[] = [1, 2, 3,]
  call print_int(count(zs),)

  var e: int{} = {\"k\": 9,}
  call print_int(e[\"k\"])
end
";
    let lines = build_run("run", src);
    assert_eq!(
        lines,
        vec![
            "2",        // 10 +=5 -=3 *=2 /=4 mod=4  -> 2
            "abc",      // "a" &= "b" &= "c"
            "1",        // increment, increment, decrement
            "foobar",   // text +
            "ababab",   // text * 3
            "t1",       // 1 <= 5 <= 12 is true
            "f1",       // 1 <= 20 <= 12 is false (20 <= 12 fails)
            "mid-in",   // 1 <= bump() <= 10 is true
            "1",        // bump() ran exactly once: the middle is evaluated once
            "has20",    // 20 in [10,20,30]
            "no99",     // 99 not in [10,20,30]
            "hasA",     // "a" in the dictionary
            "noZ",      // "z" not in the dictionary
            "substr",   // "ell" in "hello"
            "nosub",    // "xyz" not in "hello"
            "fires",    // one-line if that fires
            "4",        // `increment y if y = 3` ran; the skipped one did not
            "1000000",  // 1_000_000
            "3",        // count of [1, 2, 3,] with a trailing comma, called with one too
            "9",        // {"k": 9,} with a trailing comma
        ]
    );
}

/// `+` joins text with text and refuses to stringify a number — turning a value
/// into its text is interpolation's job, not `+`'s.
#[test]
fn text_plus_a_number_is_a_build_error() {
    let stderr = build_fails(
        "plus_num",
        "module bad\nsub main\n  call print_text(\"n=\" + 5)\nend\n",
    );
    assert!(
        stderr.contains("joins text with text"),
        "the diagnostic must explain `+` does not stringify, got:\n{stderr}"
    );
}

/// `in` tests membership in an array, a dictionary or text; a number has no
/// members, and the message says so rather than lowering to nothing.
#[test]
fn in_on_an_unsupported_type_is_a_build_error() {
    let stderr = build_fails(
        "in_bad",
        "module bad\nsub main\n  if 5 in 10\n    call print_int(1)\n  end\nend\n",
    );
    assert!(
        stderr.contains("has no members"),
        "the diagnostic must name the unsupported type, got:\n{stderr}"
    );
}
