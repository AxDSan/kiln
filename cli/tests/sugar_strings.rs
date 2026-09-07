//! End-to-end tests for 0.8.0 string interpolation: a text literal's `{expr}`
//! holes desugar to a `concat` chain that turns each hole to text by its type.
//! The output is what is proved — a desugar that type-checks but concatenates
//! the wrong pieces is the failure that matters — plus the escapes (`{{`/`}}`),
//! an all-literal string left untouched, the three positions a literal shows up
//! in (a `print_text` argument, a component property, a `return`), and the
//! build errors that keep the feature honest (an empty hole, a type with no
//! text form, and the colon reserved for a format spec that does not exist yet).
use std::path::{Path, PathBuf};
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
    let dir = std::env::temp_dir().join(format!("kiln_strings_{tag}"));
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

/// Build `src` and assert only that it BUILDS — for a program that cannot be run
/// here (a GUI), where the point is that the desugar reaches the backend and
/// type-checks.
fn build_ok(tag: &str, src: &str) {
    let dir = std::env::temp_dir().join(format!("kiln_strings_{tag}"));
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
        out.status.success(),
        "the program was expected to build but failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Build `src`, assert it FAILS, and return the compiler's stderr.
fn build_fails(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("kiln_strings_{tag}"));
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

/// One program with a hole of every convertible type, both brace escapes, a
/// string with no holes at all, and a lone hole with no surrounding text —
/// printed, so the exact desugared text is what the test checks.
const EVERY_HOLE: &str = r#"module strings

record person
  name: text
  age: int
end

sub greet(): text
  return "hi"
end

sub main
  let i: int = 3
  let n: int = 7
  let big: int64 = int_to_int64(1000000)
  let r: double = 0.5
  let ok: bool = true
  let no: bool = false
  let who: text = "Ada"
  let p: person = person(name: "Bea", age: 41)

  call print_text("Row {i} of {n}")
  call print_text("total = {i * n}")
  call print_text("big = {big}")
  call print_text("ratio {r}")
  call print_text("ok {ok} no {no}")
  call print_text("hi {who}, {greet()}")
  call print_text("{p.name} is {p.age}")
  call print_text("a literal brace: {{ and }}")
  call print_text("no holes here")
  call print_text("{i}")
end
"#;

#[test]
fn every_hole_type_prints_exactly() {
    let lines = build_run("every", EVERY_HOLE);
    assert_eq!(
        lines,
        vec![
            "Row 3 of 7",           // int holes -> int_to_text
            "total = 21",           // an expression hole, int arithmetic
            "big = 1000000",        // int64 hole -> int64_to_text
            "ratio 0.5",            // double hole -> double_to_text
            "ok true no false",     // bool holes -> the words true / false
            "hi Ada, hi",           // a text var (as is) and a call hole
            "Bea is 41",            // a field hole, text then int
            "a literal brace: { and }", // {{ and }} unescape to single braces
            "no holes here",        // no holes: the literal is untouched
            "3",                    // a lone hole: the conversion, no concat
        ],
        "the interpolated output did not match"
    );
}

#[test]
fn interpolation_in_a_return_value() {
    let src = r#"module ret

sub label_for(n: int): text
  return "count: {n}"
end

sub main
  call print_text(label_for(9))
end
"#;
    assert_eq!(build_run("ret", src), vec!["count: 9"]);
}

#[test]
fn interpolation_in_a_component_property_builds() {
    // A runtime assignment to a component property is an ordinary expression on
    // the right, so `label.text = "count: {count}"` interpolates exactly as a
    // `print_text` argument does. Built, not run: it opens a window.
    let src = r#"module ui_interp
use ui

var count: int = 0

form main_window
  title = "interp"
  width = 320
  height = 200

  label count_label
    text = "0"
    left = 20
    top = 20
    width = 260
  end

  button add_button
    text = "Add"
    left = 20
    top = 80
    width = 120
    on click: on_add
  end
end

sub on_add
  count = count + 1
  count_label.text = "count: {count}"
end
"#;
    build_ok("prop", src);
}

#[test]
fn an_empty_hole_is_a_build_error() {
    let src = r#"module empty

sub main
  call print_text("nothing in {} here")
end
"#;
    let err = build_fails("empty", src);
    assert!(
        err.contains("empty interpolation hole"),
        "expected an empty-hole error, got:\n{err}"
    );
}

#[test]
fn a_type_with_no_text_form_is_a_build_error() {
    // A `ptr` has no text form, so a hole holding one is refused — and the error
    // names the hole so the author knows which one.
    let src = r#"module noptr

sub main
  let p: ptr = ptr_null()
  call print_text("addr is {p}")
end
"#;
    let err = build_fails("noptr", src);
    assert!(
        err.contains("{p}") && err.contains("no text form"),
        "expected a no-text-form error naming the hole, got:\n{err}"
    );
}

#[test]
fn a_colon_in_a_hole_is_a_reserved_format_spec_error() {
    // Format specs are not implemented; the colon that would introduce one is
    // reserved and reported plainly rather than mis-parsed.
    let src = r#"module spec

sub main
  let n: int = 4
  call print_text("n = {n:04}")
end
"#;
    let err = build_fails("spec", src);
    assert!(
        err.contains("format specs") && err.contains("not supported yet"),
        "expected a reserved-format-spec error, got:\n{err}"
    );
}

// --- The same interpolation, cross-built for Windows and run under wine ------

fn on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wine_lines(image: &Path, cwd: &Path) -> Option<Vec<String>> {
    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return None;
    }
    let out = Command::new("wine")
        .arg(image)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the Windows program exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

#[test]
fn interpolation_cross_builds_and_runs_under_wine() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("mingw is not installed; skipping the Windows interpolation test");
        return;
    }
    let dir = std::env::temp_dir().join("kiln_strings_windows_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let srcpath = dir.join("prog.kiln");
    std::fs::write(
        &srcpath,
        r#"module winstrings

sub main
  let i: int = 3
  let n: int = 7
  let ok: bool = true
  call print_text("Row {i} of {n}, ok={ok}")
  call print_text("brace {{x}}")
end
"#,
    )
    .expect("write source");
    let out = dir.join("prog");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", srcpath.to_str().unwrap(), "--os", "windows", "-o"])
        .arg(&out)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln build --os windows");
    assert!(status.success(), "kiln build --os windows failed");
    let image = dir.join("prog.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());

    if let Some(lines) = wine_lines(&image, &dir) {
        assert_eq!(lines, vec!["Row 3 of 7, ok=true", "brace {x}"]);
    }
}
