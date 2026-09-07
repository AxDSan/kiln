//! End-to-end tests for the 0.9.0 literal and slicing sugar: block strings
//! (`"""..."""`), raw strings (`r"..."`), the collection literals, and the
//! slice `xs[a..b]`.
//!
//! Every one of these is a *rewrite* into something the language already had —
//! a block string is a text literal whose newlines were typed rather than
//! escaped, a raw string is one with no escape pass at all, `[a, b, c]` is
//! build-then-fill, and a slice is the `substr` / `slice` / `bytes_slice`
//! command the base's type answers to. So the tests prove the OUTPUT, byte for
//! byte: sugar that type-checks and computes the wrong thing is the failure
//! that matters. Positions count from 1 and a slice includes both ends, so the
//! expected values are spelled out rather than computed.
use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_sugar09lits_{tag}"));
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

/// Build `src`, run it, and return its stdout **verbatim** — not split into
/// lines, because a block string's whole point is the newlines in it.
fn build_run_raw(tag: &str, src: &str) -> String {
    let bin = build(tag, src).unwrap_or_else(|e| panic!("the program failed to build:\n{e}"));
    let run = Command::new(&bin).output().expect("run built binary");
    assert!(
        run.status.success(),
        "the built program exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

fn build_run(tag: &str, src: &str) -> Vec<String> {
    build_run_raw(tag, src)
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

/// A block string keeps its newlines and still fires its holes: it is a text
/// literal written over several lines, not a different kind of literal. The
/// one newline directly after the opening `"""` is dropped, which is the whole
/// of the rule — the body's indentation is its own.
#[test]
fn a_block_string_keeps_its_newlines_and_its_holes() {
    let out = build_run_raw(
        "block",
        r#"module block

sub main
  let n: int = 2
  let msg: text = """
Dear reader,
you have {n} message(s).
"""
  call print_text(msg)
  # No leading newline to drop: the body starts against the delimiter.
  call print_text("""one
two""")
  # A lone `"` inside a block is content; only three in a row close it.
  call print_text("""a "quoted" word""")
end
"#,
    );
    assert_eq!(
        out,
        "Dear reader,\nyou have 2 message(s).\n\none\ntwo\na \"quoted\" word\n",
        "the block string did not come out as written"
    );
}

/// A raw string is its own bytes: a backslash is a backslash, and a brace is a
/// brace rather than a hole. It is what a Windows path and a regular expression
/// are made of, and both forms — one line and block — behave the same way.
#[test]
fn a_raw_string_keeps_its_backslashes() {
    let out = build_run(
        "raw",
        r#"module raw

sub main
  call print_text(r"C:\logs\today.txt")
  call print_text(r"\d+\s*(\w+)")
  # No interpolation: the braces are characters.
  call print_text(r"a {hole} that is not one")
  # `\n` in a raw string is two characters, so `length` is 4, not 3.
  call print_int(length(r"a\nb"))
  # A block raw string may hold a single quote AND keep its backslashes.
  call print_text(r"""say "hi"\n""")
  # The ordinary form still escapes: this one really is a newline.
  call print_int(length("a\nb"))
end
"#,
    );
    assert_eq!(
        out,
        vec![
            r"C:\logs\today.txt",
            r"\d+\s*(\w+)",
            "a {hole} that is not one",
            "4",
            r#"say "hi"\n"#,
            "3",
        ]
    );
}

/// `[a, b, c]` builds an array with the elements in the order written, filled
/// from position 1; `{k: v}` builds a dictionary. Both work in expression
/// position — an argument, not only a `let` initializer — and an empty one
/// takes its type from the binding it is going into.
#[test]
fn collection_literals_build_what_they_say() {
    let out = build_run(
        "lits",
        r#"module lits

sub total(xs: int[]): int
  var sum: int = 0
  for each x in xs
    sum += x
  end
  return sum
end

sub main
  call print_int(total([10, 20, 30]))
  # Positions count from 1, and the fill is in the order written.
  let xs: int[] = [10, 20, 30]
  call print_int(xs[1])
  call print_int(xs[3])
  # A trailing comma is tolerated, and the elements may be any expression.
  let doubled: int[] = [xs[1] * 2, xs[2] * 2,]
  call print_int(doubled[2])
  # An empty literal, typed by its binding.
  var none: int[] = []
  call print_int(count(none))
  none = append(none, 7)
  call print_int(none[1])
  var ages: int{} = {}
  call print_int(dict_count(ages))
  let known: int{} = {"ada": 36, "alan": 41}
  call print_int(known["alan"])
  call print_int(dict_count(known))
end
"#,
    );
    assert_eq!(
        out,
        vec!["60", "10", "30", "40", "0", "7", "0", "41", "2"]
    );
}

/// `s[a..b]` is inclusive at both ends and counts from 1, over text, over a
/// byte-set, and over an array; a missing bound is the collection's own end.
/// Text is measured in CHARACTERS, so a slice of accented text does not split
/// one.
#[test]
fn a_slice_takes_a_run_from_either_end() {
    let out = build_run(
        "slice",
        r#"module slice

sub main
  let s: text = "Hello, world"
  call print_text(s[1..5])
  call print_text(s[6..])
  call print_text(s[..3])
  call print_text(s[..])
  # One position: both ends included, so `s[3..3]` is one character.
  call print_text(s[3..3])
  # Characters, not bytes.
  call print_text("héllo"[1..3])
  # An array slice is an array of the same elements.
  let xs: int[] = [10, 20, 30, 40, 50]
  let mid: int[] = xs[2..4]
  call print_int(count(mid))
  call print_int(mid[1])
  call print_int(mid[3])
  let names: text[] = ["ada", "alan", "grace"]
  call print_text(join(names[2..], "-"))
  # A byte-set slice is a byte-set.
  let b: bytes = bytes_from_text("ABCDEF")
  let run: bytes = b[2..4]
  call print_int(bytes_count(run))
  call print_text(text_from_bytes(run))
end
"#,
    );
    assert_eq!(
        out,
        vec![
            "Hello",
            ", world",
            "Hel",
            "Hello, world",
            "l",
            "hél",
            "3",
            "20",
            "40",
            "alan-grace",
            "3",
            "BCD",
        ]
    );
}

/// Out-of-range bounds are CLAMPED, never an error — the same bargain `substr`
/// has always made, and the same one for all three kinds, so `xs[a..b]` and
/// `s[a..b]` cannot disagree about the same numbers. A slice is where a program
/// asks how much is there.
#[test]
fn slice_bounds_outside_the_collection_are_clamped() {
    let out = build_run(
        "clamp",
        r#"module clamp

sub main
  let s: text = "abcdef"
  call print_text("[" + s[20..30] + "]")     # entirely past the end
  call print_text("[" + s[4..99] + "]")      # stops at the end
  call print_text("[" + s[0..3] + "]")       # a start below 1 reads from 1
  call print_text("[" + s[4..2] + "]")       # backwards is empty
  let xs: int[] = [1, 2, 3]
  call print_int(count(xs[9..12]))
  call print_int(count(xs[2..99]))
  call print_int(count(xs[0..2]))
  let b: bytes = bytes_from_text("abc")
  call print_int(bytes_count(b[9..12]))
  call print_int(bytes_count(b[0..2]))
end
"#,
    );
    assert_eq!(
        out,
        vec!["[]", "[def]", "[abc]", "[]", "0", "2", "2", "0", "2"]
    );
}

/// Each bound is evaluated exactly ONCE. The rewrite spells `from` twice (the
/// command takes a count, which is `to - from + 1`) and the base twice (its
/// length fills in a missing `to`), so this is the mistake the desugar invites:
/// a call in a bound running two times.
#[test]
fn a_slice_evaluates_its_base_and_bounds_once() {
    let out = build_run(
        "once",
        r#"module once

var calls: int = 0

sub bump(v: int): int
  calls += 1
  return v
end

sub letters(): text
  calls += 1
  return "abcdef"
end

sub main
  call print_text(letters()[bump(2)..bump(4)])
  call print_int(calls)
  # A missing `to` reads the base's length — from the value already computed,
  # not by running the call again.
  call print_text(letters()[bump(5)..])
  call print_int(calls)
end
"#,
    );
    assert_eq!(out, vec!["bcd", "3", "ef", "5"]);
}

/// The two rules a collection literal must keep strict: every element has one
/// type, and an empty one with nothing to take a type from is refused where the
/// message can say so rather than guessed at.
#[test]
fn a_mixed_or_untyped_literal_is_a_build_error() {
    let mixed = build_fails(
        "mixed",
        r#"module mixed

sub main
  let xs: int[] = [1, "two", 3]
  call print_int(count(xs))
end
"#,
    );
    assert!(
        mixed.contains("one type") && mixed.contains("int") && mixed.contains("text"),
        "the mixed-type list error should name both types:\n{mixed}"
    );

    let untyped = build_fails(
        "untyped",
        r#"module untyped

sub main
  call print_int(count([]))
end
"#,
    );
    assert!(
        untyped.contains("does not say what it holds"),
        "an untyped `[]` should say it has no element type:\n{untyped}"
    );
}

/// What cannot be sliced says so, and a bound that is not a position says so:
/// both are the checker's, so neither reaches the backend as nonsense.
#[test]
fn slicing_the_wrong_thing_is_a_build_error() {
    let scalar = build_fails(
        "noslice",
        r#"module noslice

sub main
  let n: int = 5
  call print_int(n[1..2])
end
"#,
    );
    assert!(
        scalar.contains("cannot be sliced"),
        "slicing an int should be refused by name:\n{scalar}"
    );

    let bound = build_fails(
        "badbound",
        r#"module badbound

sub main
  let s: text = "abc"
  call print_text(s[1.5..2])
end
"#,
    );
    assert!(
        bound.contains("position") && bound.contains("double"),
        "a non-`int` slice bound should name the type it got:\n{bound}"
    );
}

/// A one-line string still refuses a newline, and says which form opens one
/// that may run over lines. An unterminated block says that too, rather than
/// running to the end of the file with a syntax error somewhere else.
#[test]
fn an_unterminated_or_broken_literal_says_which_form_to_use() {
    let broken = build_fails(
        "broken",
        "module broken\n\nsub main\n  call print_text(\"one\ntwo\")\nend\n",
    );
    assert!(
        broken.contains("newline in string literal"),
        "a newline in a one-line literal should be refused:\n{broken}"
    );

    let unterminated = build_fails(
        "unterminated",
        "module unterminated\n\nsub main\n  call print_text(\"\"\"one\ntwo)\nend\n",
    );
    assert!(
        unterminated.contains("unterminated"),
        "an unclosed block string should say so:\n{unterminated}"
    );
}

/// `r` is a prefix, not a keyword: it means a raw literal only when a quote
/// follows it immediately, so a variable, a parameter or a subroutine named `r`
/// keeps working.
#[test]
fn r_is_still_usable_as_a_name() {
    let out = build_run(
        "rname",
        r#"module rname

sub r(n: int): int
  return n * 2
end

sub main
  let r: int = 21
  call print_int(r)
  call print_int(r(4))
end
"#,
    );
    assert_eq!(out, vec!["21", "8"]);
}
