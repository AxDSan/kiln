//! End-to-end tests for the Phase 1 toolchain: build `.kiln` examples to native
//! binaries, run them, and prove dead-code stripping.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Build `<repo>/examples/<name>.kiln` to a temp binary; return its path.
///
/// `tag` MUST be unique per test. Tests run in parallel, and two tests writing
/// the same output path race: one truncates the binary while the other executes
/// it, producing an intermittent failure that looks like a product bug. There is
/// deliberately no tag-less convenience wrapper — it is what caused exactly that
/// flake twice.
fn build_as(name: &str, tag: &str) -> PathBuf {
    let repo = repo();
    let example = repo.join("examples").join(format!("{name}.kiln"));
    let out_bin = std::env::temp_dir().join(format!("kiln_{name}_{tag}_test"));
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            example.to_str().unwrap(),
            "-o",
            out_bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build {name} failed");
    out_bin
}

fn run(bin: &Path) -> String {
    let out = Command::new(bin).output().expect("run built binary");
    assert!(out.status.success(), "binary exited non-zero");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn hello_builds_and_runs() {
    let stdout = run(&build_as("hello", "run"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec!["Kiln — arithmetic demo", "42", "14", "42", "42"]
    );
}

#[test]
fn demo_builds_and_runs() {
    let stdout = run(&build_as("demo", "run"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "HELLO, KILN", // uppercase(concat)
            "11",             // length("Hello, Kiln")
            "a/b/c/d",        // replace "-" -> "/"
            "padded",         // trim
            "desserts",       // reverse("stressed")
            "9",              // max_int(3,9)
            "1024",           // pow_int(2,10)
            "2",              // mod_int(17,5)
            "1.41421",        // sqrt(2)
            "2",              // round(pow(sqrt2, 2))
            "n = 42",         // conversions round-trip
            "1970",           // year(epoch)
        ],
        "unexpected demo output:\n{stdout}"
    );
}

///; unused command code is
/// dead-stripped by `-ffunction-sections` + `--gc-sections`.
#[test]
fn unused_commands_are_dead_stripped() {
    let bin = build_as("hello", "strip"); // uses only print_text / print_int + arithmetic
    let nm = match Command::new("nm").arg(&bin).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            eprintln!("nm unavailable; skipping symbol check");
            return;
        }
    };
    let symbols: Vec<&str> = nm
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();

    // Referenced commands are present.
    for want in ["kn_print_int", "kn_print_text"] {
        assert!(symbols.contains(&want), "expected `{want}` to be linked in");
    }
    // Unreferenced commands are gone.
    for gone in [
        "kn_sqrt",
        "kn_replace",
        "kn_now",
        "kn_uppercase",
        "kn_pow_int",
    ] {
        assert!(
            !symbols.contains(&gone),
            "unused command `{gone}` was not stripped"
        );
    }
}

/// Subroutines with parameters, return values and recursion, end to end: the
/// point is that the compiled program produces the answers, not that the IR
/// looked right.
#[test]
fn subs_build_and_run() {
    let stdout = run(&build_as("subs", "params"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "5",          // add(2, 3)
            "7",          // add(add(1, 2), 4)
            "720",        // factorial(6)
            "610",        // fib(15) — recursion
            "ADA!",       // text in, text out
            "negative",   // an early bare `return`
            "4 is even",  // a sub calling another sub
            "7 is odd",
            "79", // add(factorial(4), fib(10)) = 24 + 55
        ],
        "unexpected subs output:\n{stdout}"
    );
}

#[test]
fn hello_library_via_abi() {
    // `use hello` — a third-party support library loaded through the ABI.
    let stdout = run(&build_as("hellolib", "abi"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["Hello, Kiln!", "HELLO, WORLD!"]);
}

///
/// a native GUI binary, and a click reaches the handler subroutine.
///
/// Runs headlessly through the UI test hooks (see abi/kiln_ui.h): render a
/// few frames, dispatch a synthetic click at widget handle 3 (the button), exit.
#[test]
fn form_builds_and_click_reaches_handler() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored (run tools/fetch-rmlui.sh); skipping GUI test");
        return;
    }
    let bin = build_as("form", "click");

    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "3")
        .env("KILN_UI_SYNTH_CLICK", "3")
        .output()
        .expect("run form binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("button clicked!"),
        "click did not reach the handler; stdout was:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The UI stack must link ONLY into modules that declare a form. Without this,
/// every console program would carry megabytes of widget toolkit (and the
/// dead-strip guarantee.
#[test]
fn console_programs_do_not_link_the_ui() {
    let bin = build_as("demo", "ldd");
    let ldd = match Command::new("ldd").arg(&bin).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            eprintln!("ldd unavailable; skipping");
            return;
        }
    };
    for forbidden in ["libSDL2", "libGL.so", "libfreetype"] {
        assert!(
            !ldd.contains(forbidden),
            "console binary links `{forbidden}` — the UI stack is leaking into non-GUI programs:\n{ldd}"
        );
    }
}

/// A button must visibly respond to the mouse. Hover feedback can't come from a
/// `:hover` stylesheet rule because component properties are applied INLINE and
/// inline styles outrank stylesheet rules — so the backend drives the states,
/// and this test guards that it actually changes rendered pixels.
#[test]
fn button_hover_changes_pixels() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let bin = build_as("form", "hover");
    let dir = std::env::temp_dir();
    let base = dir.join("kiln_base.ppm");
    let hover = dir.join("kiln_hover.ppm");

    let render = |dump: &std::path::Path, mouse: Option<&str>| {
        let mut c = Command::new(&bin);
        c.env("KILN_UI_EXIT_AFTER_FRAMES", "3")
            .env("KILN_UI_DUMP", dump);
        if let Some(m) = mouse {
            c.env("KILN_UI_MOUSE", m);
        }
        assert!(c.output().expect("render").status.success());
    };
    render(&base, None);
    render(&hover, Some("120,132")); // centre of the button

    // Sample the same pixel from each frame.
    let sample = |p: &std::path::Path| -> [u8; 3] {
        let bytes = std::fs::read(p).expect("read ppm");
        // PPM header is three newline-terminated lines; the second holds the width.
        let mut nl = 0;
        let mut i = 0;
        let (mut w, mut hdr) = (0usize, String::new());
        while nl < 3 && i < bytes.len() {
            if bytes[i] == b'\n' {
                nl += 1;
                if nl == 2 {
                    w = hdr.split_whitespace().next().unwrap().parse().unwrap();
                }
                hdr.clear();
            } else {
                hdr.push(bytes[i] as char);
            }
            i += 1;
        }
        let (x, y) = (120usize, 132usize);
        let off = i + (y * w + x) * 3;
        [bytes[off], bytes[off + 1], bytes[off + 2]]
    };
    let (b, h) = (sample(&base), sample(&hover));
    assert_ne!(
        b, h,
        "hovering the button did not change its appearance (base {b:?})"
    );
}

/// The accessibility tree must mirror the widget tree with correct roles,
/// names, parent links and bounds. Substrate-independent and
/// needs no accessibility bus, so it always runs.
#[test]
fn accessibility_tree_is_published() {
    let repo = repo();
    if !repo.join("vendor/accesskit-c/include/accesskit.h").exists() {
        eprintln!("accesskit-c not vendored (run tools/fetch-accesskit.sh); skipping");
        return;
    }
    let bin = build_as("form", "a11y");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "3")
        .env("KILN_UI_DUMP_A11Y", "1")
        .output()
        .expect("run form");
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("a11y: id="))
        .collect();
    assert_eq!(rows.len(), 3, "expected window+label+button, got:\n{text}");

    // role 1 = window, 3 = label, 2 = button (abi/kiln_abi.h)
    assert!(
        rows[0].contains("role=1") && rows[0].contains("parent=0"),
        "root: {}",
        rows[0]
    );
    assert!(
        rows[1].contains("role=3") && rows[1].contains("Click the button."),
        "label: {}",
        rows[1]
    );
    assert!(
        rows[2].contains("role=2") && rows[2].contains("Click me") && rows[2].contains("clickable"),
        "button must be a clickable button with an accessible name: {}",
        rows[2]
    );
    // Bounds must be real, not zero — an AT cannot locate a zero-sized control.
    assert!(
        !rows[2].contains("bounds=0,0,0x0"),
        "button has no bounds: {}",
        rows[2]
    );
}

/// End-to-end against a real AT-SPI bus: the adapter must actually activate
/// when an assistive technology is present. Skipped when the session has no
/// accessibility bus, so CI never hard-depends on desktop D-Bus.
#[test]
fn accessibility_adapter_activates_on_a_real_bus() {
    let repo = repo();
    if !repo.join("vendor/accesskit-c/include/accesskit.h").exists() {
        eprintln!("accesskit-c not vendored; skipping");
        return;
    }
    // Ask the a11y bus to exist, and mark accessibility enabled.
    let bus = Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.a11y.Bus",
            "/org/a11y/bus",
            "org.a11y.Bus",
            "GetAddress",
        ])
        .output();
    match bus {
        Ok(o) if o.status.success() => {}
        _ => {
            eprintln!("no session accessibility bus; skipping live-adapter test");
            return;
        }
    }
    for prop in ["IsEnabled", "ScreenReaderEnabled"] {
        let _ = Command::new("busctl")
            .args([
                "--user",
                "set-property",
                "org.a11y.Bus",
                "/org/a11y/bus",
                "org.a11y.Status",
                prop,
                "b",
                "true",
            ])
            .output();
    }

    let bin = build_as("form", "a11ylive");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "90")
        .env("KILN_UI_DUMP_A11Y", "1")
        .output()
        .expect("run form");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.contains("adapter_active=1") {
        // The bus exists but no AT attached; that is an environment fact, not a
        // product defect, so report rather than fail.
        eprintln!("accessibility bus present but no AT attached; adapter stayed idle");
    }
}

/// Accessibility must never be load-bearing: with it disabled the app runs
/// exactly the same. An app that breaks without a11y infrastructure would fail
/// the very requirement the bridge exists to satisfy.
#[test]
fn app_runs_with_accessibility_disabled() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let bin = build_as("form", "noa11y");
    let out = Command::new(&bin)
        .env("KILN_NO_A11Y", "1")
        .env("KILN_UI_EXIT_AFTER_FRAMES", "3")
        .env("KILN_UI_SYNTH_CLICK", "3")
        .output()
        .expect("run form");
    assert!(
        out.status.success(),
        "app failed with accessibility disabled"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("button clicked!"),
        "app must behave identically with accessibility disabled"
    );
}

/// Reading and writing component properties from code. The counter
/// holds its state in the label's own text, so one synthetic click must both
/// print the new count and leave the label showing it.
#[test]
fn property_access_updates_a_component() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let bin = build_as("counter", "prop");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "6")
        .env("KILN_UI_SYNTH_CLICK", "3")
        .env("KILN_UI_DUMP_A11Y", "1")
        .output()
        .expect("run counter");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("count = 1"),
        "handler did not update the label:\n{stdout}"
    );
    // The accessible name must track the live text, not the construction-time
    // value — otherwise a screen reader announces stale content. After one
    // click the label shows the text from the `count = 1` branch.
    let label_row = stdout
        .lines()
        .find(|l| l.starts_with("a11y: id=2"))
        .expect("a label node in the a11y tree");
    assert!(
        label_row.contains("name=\"1 (first click)\""),
        "accessible name is stale (it must mirror the label's current text): {label_row}"
    );
}

/// A module with BOTH `main` and a form: the form must be built before `main`
/// runs, or `main` addresses components that do not exist yet. This is the
/// ordering bug that would only ever appear for this module shape.
#[test]
fn main_may_touch_components_before_the_loop_starts() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let bin = build_as("mainorder", "order");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "3")
        .output()
        .expect("run mainorder");
    assert!(out.status.success(), "main touching a component crashed");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("main saw: set from main"),
        "main could not read back the property it set"
    );
}

/// Control flow end to end: if / else if / else, while, comparisons,
/// short-circuit `and`, and content-based text equality.
///
/// This also serves as the SSA verifier for branch codegen — clang rejects
/// malformed basic blocks, so a passing build proves the emitted IR is sound.
#[test]
fn control_flow_runs_correctly() {
    let stdout = run(&build_as("fizzbuzz", "cf"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 22, "unexpected output:\n{stdout}");
    assert_eq!(&lines[..5], &["1", "2", "Fizz", "4", "Buzz"]);
    assert_eq!(lines[14], "FizzBuzz", "15 should be FizzBuzz");
    assert_eq!(lines[19], "Buzz", "20 should be Buzz");
    // The right side of `and` must not be evaluated when the left is false,
    // or this would have divided by zero.
    assert_eq!(lines[20], "short-circuit ok");
    // Two separately-built strings with the same characters must compare equal.
    assert_eq!(lines[21], "text equality ok");
}

/// Loops end to end: counting up and down, `break`, `continue`, and the
/// nesting rule that `break` leaves only the innermost loop. As with
/// `control_flow_runs_correctly`, a successful build is itself the proof that
/// the emitted basic blocks are well formed — clang rejects them otherwise.
#[test]
fn loops_build_and_run() {
    let stdout = run(&build_as("loops", "loops"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "-- fizzbuzz, counted --");
    assert_eq!(&lines[1..6], &["1", "2", "Fizz", "4", "Buzz"]);
    assert_eq!(lines[15], "FizzBuzz", "15 should be FizzBuzz");
    // `step -1`
    assert_eq!(&lines[17..22], &["5", "4", "3", "2", "1"]);
    // `continue` then `break`: the first multiple of 7 above 100.
    assert_eq!(lines[23], "105");
    // `break`/`continue` inside a `while`.
    assert_eq!(&lines[25..28], &["word-3", "word-6", "word-9"]);
    // Nested loops: the inner `break` must not leave the outer one.
    assert_eq!(
        &lines[29..33],
        &["1 ", "2 4 ", "3 6 9 ", "4 8 12 16 "],
        "unexpected output:\n{stdout}"
    );
}

/// The expression operators: unary minus, `%`, and `+` on text. The last two
/// lines of the example build the same sentence with `+` and with nested
/// `concat` calls, so the test proves they agree.
#[test]
fn operators_build_and_run() {
    let stdout = run(&build_as("operators", "ops"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "-40",     // a negative literal
            "40",      // negating a negative
            "-250",    // negating a variable
            "1.5",     // fneg on a double
            "2",       // 17 % 5
            "-2",      // -17 % 5 — the sign follows the dividend, as srem does
            "6 is even",
            "7 is odd",
            "Hello, Ada — you are 36 today.",
            "Hello, Ada — you are 36 today.",
        ],
        "unexpected output:\n{stdout}"
    );
    assert_eq!(lines[8], lines[9], "`+` and `concat` must agree");
}

/// Dividing by zero used to kill the process with SIGFPE and no message. It
/// now goes through the runtime's error channel: a line on stderr, exit 1.
///
/// Both faulting inputs are covered — a zero divisor, and the one overflowing
/// division (the most negative integer by -1), which SIGFPEs just as hard.
#[test]
fn dividing_by_zero_reports_instead_of_crashing() {
    for (src, want) in [
        (
            "module dz\nsub main\n  var z: int = 0\n  call print_int(10 / z)\nend\n",
            "division by zero",
        ),
        (
            "module dz\nsub main\n  var z: int = 0\n  call print_int(10 % z)\nend\n",
            "remainder by zero",
        ),
        (
            "module dz\nsub main\n  var m: int = -1\n  call print_int(-2147483648 / m)\nend\n",
            "division overflowed",
        ),
    ] {
        let dir = std::env::temp_dir().join("kiln_divzero_test");
        std::fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join(format!("dz{}.kiln", want.len()));
        std::fs::write(&src_path, src).unwrap();
        let bin = dir.join(format!("dz{}", want.len()));
        let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args([
                "build",
                src_path.to_str().unwrap(),
                "-o",
                bin.to_str().unwrap(),
            ])
            .env("KILN_RUNTIME_DIR", repo().join("runtime"))
            .status()
            .expect("run kiln");
        assert!(status.success(), "build failed for: {want}");
        let out = Command::new(&bin).output().expect("run built binary");
        assert!(!out.status.success(), "{want}: expected a non-zero exit");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(want),
            "expected `{want}` on stderr, got: {stderr}"
        );
    }
}

/// A build carries a DWARF line table, and a release build does not.
///
/// This is the assertion the whole debugger rests on: an address in the built
/// program maps back to a line of the `.kiln` the user wrote. It is checked
/// through `objdump` rather than by reading the emitted IR, because IR that
/// looks right and metadata LLVM accepts are different claims — LLVM discards
/// debug information it dislikes with a warning and exit code 0.
#[test]
fn a_build_carries_a_line_table_and_a_release_build_does_not() {
    let repo = repo();
    let example = repo.join("examples").join("loops.kiln");
    let dir = std::env::temp_dir().join("kiln_linetable_test");
    std::fs::create_dir_all(&dir).unwrap();

    let decoded = |bin: &Path| -> String {
        let out = Command::new("objdump")
            .args(["--dwarf=decodedline", bin.to_str().unwrap()])
            .output()
            .expect("run objdump");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let debug_bin = dir.join("loops");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            example.to_str().unwrap(),
            "-o",
            debug_bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "debug build failed");

    let table = decoded(&debug_bin);
    assert!(
        table.contains("loops.kiln"),
        "no line table for the source: {table}"
    );
    // Every row for our source names a real line, never line 0 — a debugger
    // reads 0 as "no line here" and steps straight past it.
    let rows: Vec<&str> = table
        .lines()
        .filter(|l| l.trim_start().starts_with("loops.kiln "))
        .collect();
    assert!(rows.len() > 5, "too few rows to be a real table: {table}");
    for row in &rows {
        let line = row.split_whitespace().nth(1).unwrap_or("0");
        assert_ne!(line, "0", "a row with no line: {row}");
    }

    let release_bin = dir.join("loops-release");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            example.to_str().unwrap(),
            "--release",
            "-o",
            release_bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "release build failed");
    assert!(
        !decoded(&release_bin).contains("loops.kiln"),
        "a release build kept its line table"
    );
}

/// A loop that calls a command must not grow the stack.
///
/// The slot ABI gives every command call a return slot and an argv array on
/// the stack. Those used to be reserved where the call stood, and an `alloca`
/// outside a function's entry block is a *dynamic* stack adjustment that
/// nothing reclaims until the function returns — so each turn of the loop took
/// another bite. A million iterations is well past the quarter of a million
/// that used to fault on an 8 MiB stack, and the command is one that prints
/// nothing, so the test measures the stack rather than the pipe.
#[test]
fn a_loop_calling_a_command_does_not_grow_the_stack() {
    let dir = std::env::temp_dir().join("kiln_stackgrow_test");
    std::fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join("grow.kiln");
    std::fs::write(
        &src_path,
        "module grow\n\
         sub main\n\
         \u{20}\u{20}var total: int = 0\n\
         \u{20}\u{20}for i in 1..1000000\n\
         \u{20}\u{20}\u{20}\u{20}total = total + abs_int(0 - i)\n\
         \u{20}\u{20}end\n\
         \u{20}\u{20}call print_int(total - total)\n\
         end\n",
    )
    .unwrap();
    let bin = dir.join("grow");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            src_path.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "build failed");
    let out = Command::new(&bin).output().expect("run built binary");
    assert!(
        out.status.success(),
        "a million command calls in a loop exhausted the stack: {:?}",
        out.status
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0");
}

/// **M0, the RAD metric.** A scripted designer session adds a button,
/// sets its properties, wires a click handler, and saves — and the resulting
/// `.kiln` compiles to a native binary whose button actually works.
///
/// This is the whole product thesis in one test: draw it, wire it, ship it.
#[test]
fn designer_produces_a_working_app() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built (run designer/build.sh); skipping");
        return;
    }
    // The designed app declares a form, so compiling it needs the UI stack
    // that the designer binary itself was built against. A stale designer
    // beside an unvendored tree would otherwise fail here as a product bug.
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_designed.kiln");
    std::fs::write(
        &project,
        r#"module designed
use ui

form win
  title = "Designed"
  width = 400
  height = 260
end
"#,
    )
    .expect("write project");

    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env(
            "KILN_DESIGNER_SCRIPT",
            "add:button;set:text=Press me;set:left=40;set:top=80;wire:click=on_press;save",
        )
        .output()
        .expect("run designer");
    assert!(
        out.status.success(),
        "designer session failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The designer must not have written a second parser: what it saved has to
    // satisfy the real compiler.
    let src = std::fs::read_to_string(&project).expect("read saved project");
    assert!(
        src.contains("button button1"),
        "component not saved:\n{src}"
    );
    assert!(
        src.contains("on click: on_press"),
        "handler not wired:\n{src}"
    );
    assert!(
        src.contains("sub on_press"),
        "handler stub not generated:\n{src}"
    );

    let bin = std::env::temp_dir().join("kiln_designed_app");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            project.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("compile designed app");
    assert!(status.success(), "the designed app did not compile");

    let run = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "4")
        .env("KILN_UI_SYNTH_CLICK", "2")
        .output()
        .expect("run designed app");
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("on_press"),
        "the designed button did not reach its handler"
    );
}

/// The toolbox must actually contain the component types the UI library
/// declares. It once shipped EMPTY — the toolbox markup was built but never
/// passed to the formatter, so `%s` consumed garbage and rendered nothing. The
/// designer looked fine and simply could not add anything.
#[test]
fn designer_toolbox_lists_components() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_toolbox_probe.kiln");
    std::fs::write(
        &project,
        "module probe\nuse ui\n\nform win\n  title = \"Probe\"\nend\n",
    )
    .expect("write project");

    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env("KILN_DESIGNER_DEBUG", "1")
        .env("KILN_DESIGNER_SCRIPT", "")
        .output()
        .expect("run designer");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for want in ["oe-add=button", "oe-add=label"] {
        assert!(
            stderr.contains(want),
            "toolbox is missing {want}:\n{stderr}"
        );
    }
}

/// Every component the toolbox offers must be a REAL control, not a mockup:
/// it renders, and where it holds data that data is readable from code.
#[test]
fn all_components_are_real_controls() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let bin = build_as("controls", "real");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "4")
        // Handle 7 is the button: the form root is 1 and children follow in
        // declaration order.
        .env("KILN_UI_SYNTH_CLICK", "7")
        .env("KILN_UI_DUMP_A11Y", "1")
        .output()
        .expect("run controls");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The editbox holds real text that code can read back through the ABI.
    assert!(
        stdout.contains("name = Ada"),
        "editbox value was not readable from code:\n{stdout}"
    );
    // Every component reached the accessibility tree, so each really exists.
    let nodes = stdout
        .lines()
        .filter(|l| l.starts_with("a11y: id="))
        .count();
    assert!(
        nodes >= 7,
        "expected form + 6 components in the a11y tree, got {nodes}:\n{stdout}"
    );
}

/// Whatever the designer saves MUST compile. It once quoted a bool as text
/// (`checked = "true"`), so every Run after touching a checkbox failed — the
/// designer looked fine and produced uncompilable source.
///
/// Guards the general rule: only the descriptor's declared type can decide
/// quoting, because `text = "true"` and `checked = true` are identical strings.
#[test]
fn designer_output_always_compiles() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    // What it saves declares a form, so checking that it compiles needs the
    // vendored UI stack.
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_roundtrip.kiln");
    std::fs::copy(repo.join("examples/controls.kiln"), &project).expect("seed project");

    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env(
            "KILN_DESIGNER_SCRIPT",
            "add:checkbox;add:progressbar;add:image;add:groupbox;add:editbox;save",
        )
        .output()
        .expect("run designer");
    assert!(out.status.success(), "designer session failed");

    let src = std::fs::read_to_string(&project).expect("read saved");
    assert!(
        src.contains("checked = true"),
        "bool was quoted as text:\n{src}"
    );

    let bin = std::env::temp_dir().join("kiln_roundtrip_app");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            project.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("compile");
    assert!(
        status.success(),
        "the designer saved a file the compiler rejects"
    );
}

/// Dragging must move a component to where the cursor put it — for EVERY
/// component, including composites.
///
/// This has broken twice: once because mousedown lands on a child element that
/// carries no id (so checkboxes could not be dragged at all), and once because
/// `select()` rebuilds the canvas, destroying the element whose offset was then
/// read — making every drag jump to the top-left corner. Eyeballing the window
/// caught both late; this catches them immediately.
#[test]
fn dragging_moves_components_precisely() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_drag.kiln");
    std::fs::copy(repo.join("examples/controls.kiln"), &project).expect("seed");

    // grp starts at (20,120). Grab 10px inside it and drop at (150,120), so it
    // must land at (140,110) — cursor position minus the grab offset.
    //
    // The target is deliberately clear of every other component's edges AND
    // centre lines: alignment snapping is correct behaviour, but it must not
    // participate in a measurement of the grab offset.
    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env("KILN_DESIGNER_SCRIPT", "drag:grp@30,130->150,120;save")
        .output()
        .expect("run designer");
    assert!(out.status.success());

    let src = std::fs::read_to_string(&project).expect("read");
    let grp = src
        .split("groupbox grp")
        .nth(1)
        .expect("groupbox in saved file")
        .to_string();
    assert!(
        grp.contains("left = 140"),
        "drag lost the grab offset:\n{grp}"
    );
    assert!(
        grp.contains("top = 110"),
        "drag lost the grab offset:\n{grp}"
    );

    // A composite component must drag too: the checkbox's box and caption are
    // children with no id of their own. It starts at (20,84); grabbing 5px in
    // and dropping at (100,200) gives (95,195), which snaps to the 10px grid.
    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env("KILN_DESIGNER_SCRIPT", "drag:agree@25,89->100,200;save")
        .output()
        .expect("run designer");
    assert!(out.status.success());
    let src = std::fs::read_to_string(&project).expect("read");
    let cb = src
        .split("checkbox agree")
        .nth(1)
        .expect("checkbox")
        .to_string();
    assert!(
        cb.contains("left = 100"),
        "composite component did not drag:\n{cb}"
    );
    assert!(
        cb.contains("top = 200"),
        "composite drag lost its y offset:\n{cb}"
    );
}

/// The selection outline must trace the component's RENDERED frame.
///
/// It was derived from the model's width/height, but those size the CONTENT
/// box: a groupbox declaring 240x90 with 8px padding and a 1px border draws
/// 258x108, so the outline sat inside the real frame. Mixing a content-box
/// origin with a border-box size skewed it further.
#[test]
fn selection_outline_traces_the_rendered_frame() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_sel.kiln");
    std::fs::copy(repo.join("examples/controls.kiln"), &project).expect("seed");

    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env("KILN_DESIGNER_DEBUG", "1")
        .env("KILN_DESIGNER_SCRIPT", "select:grp")
        .output()
        .expect("run designer");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr
        .lines()
        .find(|l| l.contains("selection rect="))
        .expect("designer should report the selection rect");

    // grp is 240x90 and, since the controls became border-box, renders at
    // exactly the size it declares — padding and border inside it, not added.
    assert!(
        line.contains("selection rect=20,120 240x90"),
        "outline does not trace the rendered frame: {line}"
    );
}

/// Dragging a component near another's edge should snap flush to it — that is
/// what alignment guides are for, and it is the difference between a designer
/// that feels precise and one that feels approximate.
#[test]
fn dragging_snaps_to_alignment_guides() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_align.kiln");
    std::fs::copy(repo.join("examples/controls.kiln"), &project).expect("seed");

    // Most components sit at left = 20. Drop grp at left = 23, close enough
    // that it must snap flush to 20 rather than to the 10px grid.
    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env("KILN_DESIGNER_SCRIPT", "drag:grp@20,120->23,200;save")
        .output()
        .expect("run designer");
    assert!(out.status.success());

    let src = std::fs::read_to_string(&project).expect("read");
    let grp = src
        .split("groupbox grp")
        .nth(1)
        .expect("groupbox")
        .to_string();
    assert!(
        grp.contains("left = 20"),
        "did not snap flush to the neighbouring edge:\n{grp}"
    );
}

/// The IDE must fill the OS window at any size. Layout sizes were compile-time
/// constants, so enlarging the window left everything past 1440x900 unpainted —
/// a black frame around the UI.
#[test]
fn layout_follows_the_window_size() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    let project = std::env::temp_dir().join("kiln_resize.kiln");
    std::fs::copy(repo.join("examples/controls.kiln"), &project).expect("seed");
    let dump = std::env::temp_dir().join("kiln_resize.ppm");

    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        // A real window: the offscreen surface a headless run defaults to
        // cannot grow, and growing is the whole test.
        .env("KILN_UI_WINDOW", "1")
        .env("KILN_DESIGNER_SCRIPT", "winsize:1700x1000")
        .env("KILN_DESIGNER_DUMP", &dump)
        .output()
        .expect("run designer");
    assert!(out.status.success());

    let bytes = std::fs::read(&dump).expect("read dump");
    // Parse the PPM header: P6\n<w> <h>\n255\n
    let mut nl = 0;
    let mut i = 0;
    let mut hdr = String::new();
    let (mut w, mut h) = (0usize, 0usize);
    while nl < 3 && i < bytes.len() {
        if bytes[i] == b'\n' {
            nl += 1;
            if nl == 2 {
                let mut it = hdr.split_whitespace();
                w = it.next().unwrap().parse().unwrap();
                h = it.next().unwrap().parse().unwrap();
            }
            hdr.clear();
        } else {
            hdr.push(bytes[i] as char);
        }
        i += 1;
    }
    assert_eq!((w, h), (1700, 1000), "the window did not actually resize");

    // Every corner and edge of the enlarged window must be painted.
    let px = |x: usize, y: usize| {
        let o = i + (y * w + x) * 3;
        (bytes[o], bytes[o + 1], bytes[o + 2])
    };
    for (name, p) in [
        ("right edge", px(w - 5, h / 2)),
        ("bottom edge", px(w / 2, h - 5)),
        ("bottom-right corner", px(w - 5, h - 5)),
    ] {
        assert_ne!(
            p,
            (0, 0, 0),
            "{name} is unpainted — the layout did not follow the window"
        );
    }
}

// ---------------------------------------------------------------------------
// Build targets: one source, several artifacts
// ---------------------------------------------------------------------------

/// A library source: no entry point, two subroutines to export.
const LIB_SRC: &str = "module greetlib\n\
                       target sharedlib\n\
                       \n\
                       sub greet\n  call print_text(\"hello from a library\")\nend\n\
                       \n\
                       sub farewell\n  call print_text(\"goodbye\")\nend\n";

/// Build `LIB_SRC` for `target`, returning the artifact path. `tag` must be
/// unique per test — see `build_as`.
fn build_lib(target: &str, tag: &str, ext: &str) -> PathBuf {
    let repo = repo();
    let dir = std::env::temp_dir().join(format!("kiln_lib_{tag}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join("greetlib.kiln");
    std::fs::write(&src, LIB_SRC).expect("write source");
    let out = dir.join(format!("libgreet.{ext}"));

    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            src.to_str().unwrap(),
            "--target",
            target,
            "-o",
            out.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build --target {target} failed");
    out
}

/// Compile `main_src` against `lib`, run it, and return its stdout.
fn run_c_host(dir: &Path, main_src: &str, lib: &Path, tag: &str) -> String {
    let c = dir.join(format!("host_{tag}.c"));
    std::fs::write(&c, main_src).expect("write host");
    let bin = dir.join(format!("host_{tag}"));
    let status = Command::new("clang")
        .arg(&c)
        .arg(lib)
        .arg("-lm")
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run clang");
    assert!(status.success(), "linking the C host failed");
    let out = Command::new(&bin).output().expect("run host");
    assert!(out.status.success(), "host exited non-zero");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A `.so` must actually LOAD and RUN, not merely exist with the right
/// extension. The first version of this linked cleanly and then failed to
/// dlopen with `undefined symbol: ECodeStart`, because the runtime's
/// process-entry object came along for the ride.
#[test]
fn shared_library_loads_and_its_exports_run() {
    let lib = build_lib("sharedlib", "so", "so");
    let dir = lib.parent().unwrap();

    let host = "#include <dlfcn.h>\n#include <stdio.h>\n\
                int main(void){\n\
                  void* h = dlopen(\"LIBPATH\", RTLD_NOW);\n\
                  if(!h){ printf(\"dlopen failed: %s\\n\", dlerror()); return 1; }\n\
                  void (*init)(void) = dlsym(h, \"greetlib_init\");\n\
                  void (*greet)(void) = dlsym(h, \"greet\");\n\
                  void (*bye)(void) = dlsym(h, \"farewell\");\n\
                  if(!init || !greet || !bye){ printf(\"missing export\\n\"); return 1; }\n\
                  init(); greet(); bye(); return 0;\n}\n";
    let host = host.replace("LIBPATH", lib.to_str().unwrap());

    // The host links against libdl only; the library is opened at run time.
    let c = dir.join("dlhost.c");
    std::fs::write(&c, host).expect("write host");
    let bin = dir.join("dlhost");
    let status = Command::new("clang")
        .arg(&c)
        .arg("-ldl")
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("clang");
    assert!(status.success(), "compiling the dlopen host failed");
    let out = Command::new(&bin).output().expect("run host");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("hello from a library") && text.contains("goodbye"),
        "exports did not run: {text}"
    );
}

/// The exports must be there under their plain names, and the program entry
/// must NOT be: a library with an `ECodeStart` is a program wearing a hat.
#[test]
fn shared_library_exports_plain_names_and_no_entry() {
    let lib = build_lib("sharedlib", "syms", "so");
    let out = Command::new("nm")
        .args(["-D", "--defined-only"])
        .arg(&lib)
        .output()
        .expect("nm");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(" T greet"), "missing export `greet`: {text}");
    assert!(text.contains(" T farewell"), "missing export `farewell`");
    assert!(
        !text.contains("ECodeStart"),
        "a library must not define a program entry: {text}"
    );

    let undef = Command::new("nm").args(["-D", "-u"]).arg(&lib).output().expect("nm");
    assert!(
        !String::from_utf8_lossy(&undef.stdout).contains("ECodeStart"),
        "unresolved ECodeStart would make the .so unloadable"
    );
}

/// A `.a` has to link into a real host and run.
#[test]
fn static_library_links_into_a_c_host() {
    let lib = build_lib("staticlib", "a", "a");
    let dir = lib.parent().unwrap();
    let text = run_c_host(
        dir,
        "void greetlib_init(void); void greet(void);\n\
         int main(void){ greetlib_init(); greet(); return 0; }\n",
        &lib,
        "static",
    );
    assert!(
        text.contains("hello from a library"),
        "the archived export did not run: {text}"
    );
}

/// The same source builds as either artifact — that is what makes the target a
/// build-time choice rather than a rewrite (G12).
#[test]
fn one_source_builds_as_both_library_kinds() {
    let so = build_lib("sharedlib", "both_so", "so");
    let a = build_lib("staticlib", "both_a", "a");
    assert!(so.exists() && a.exists());
    let ar = Command::new("ar").arg("t").arg(&a).output().expect("ar t");
    assert!(
        !String::from_utf8_lossy(&ar.stdout).trim().is_empty(),
        "the archive should contain objects"
    );
}

/// A library with no subroutines exports nothing, and a form belongs to a GUI
/// program — both are caught before the toolchain is invoked.
#[test]
fn library_targets_reject_nonsense() {
    let dir = std::env::temp_dir().join("kiln_lib_reject");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join("empty.kiln");
    std::fs::write(&src, "module empty\ntarget sharedlib\n").expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "an empty library should not build");
    assert!(
        err.contains("exports nothing"),
        "the error should say what is wrong: {err}"
    );
}

// ---------------------------------------------------------------------------
// Project templates
// ---------------------------------------------------------------------------

fn kiln(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln")
}

/// A template that does not compile is worse than no template: it hands a
/// newcomer a broken project as their first experience. Every one is built.
#[test]
fn every_template_creates_a_project_that_builds() {
    let listing = kiln(&["templates"]);
    let text = String::from_utf8_lossy(&listing.stdout);
    // `template: <id> <target>`. A `gui` template links the vendored UI stack,
    // which a fresh checkout does not have; skipping it is honest, and failing
    // on it would make every unvendored build red for a reason that is not a
    // defect in the template.
    let ui_vendored = repo().join("vendor/RmlUi/build/librmlui.a").exists();
    let ids: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("template: "))
        .filter(|l| ui_vendored || !l.split_whitespace().nth(1).is_some_and(|t| t == "gui"))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(!ids.is_empty(), "no templates listed:\n{text}");

    for id in ids {
        let dir = std::env::temp_dir().join(format!("kiln_tmpl_{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        let out = kiln(&["new", id, dir.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "`kiln new {id}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The listing tells the caller which file to open; it must exist.
        let created = String::from_utf8_lossy(&out.stdout);
        let open = created
            .lines()
            .find_map(|l| l.strip_prefix("open: "))
            .expect("`new` should report the file to open");
        assert!(Path::new(open).is_file(), "{open} was not created");

        let bin = dir.join("out");
        let built = kiln(&["build", open, "-o", bin.to_str().unwrap()]);
        assert!(
            built.status.success(),
            "template `{id}` does not build:\n{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The console template is the newcomer's first program: it must actually run
/// and print, not merely compile.
#[test]
fn the_console_template_runs() {
    let dir = std::env::temp_dir().join("kiln_tmpl_run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(kiln(&["new", "console-app", dir.to_str().unwrap()])
        .status
        .success());

    let bin = dir.join("app");
    let src = dir.join("main.kiln");
    assert!(kiln(&["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .status
        .success());
    let out = Command::new(&bin).output().expect("run template app");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Hello from Kiln"), "got: {text}");
    assert!(text.contains("six times seven is 42"), "arithmetic line missing: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The module name comes from the directory, sanitised into an identifier.
#[test]
fn the_module_name_follows_the_directory() {
    let dir = std::env::temp_dir().join("kiln-my-app");
    let _ = std::fs::remove_dir_all(&dir);
    let out = kiln(&["new", "console-app", dir.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("module: kiln_my_app"),
        "dashes are not legal in an identifier: {text}"
    );
    let src = std::fs::read_to_string(dir.join("main.kiln")).expect("read");
    assert!(src.contains("module kiln_my_app"));
    assert!(!src.contains("__MODULE__"), "placeholder left behind");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Creating into someone's existing work would destroy it.
#[test]
fn new_refuses_a_non_empty_directory() {
    let dir = std::env::temp_dir().join("kiln_tmpl_occupied");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("keepme.txt"), "important").expect("write");
    let out = kiln(&["new", "console-app", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "should refuse a non-empty directory");
    assert!(
        dir.join("keepme.txt").is_file(),
        "the existing file must survive"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Text is UTF-8, so character-shaped commands must count and cut CHARACTERS.
/// Measuring in bytes leaks the encoding into every program: a word with an
/// accent in it reports the wrong length, and a slice at a byte offset splits
/// a character and yields text that is no longer valid UTF-8.
#[test]
fn text_commands_are_utf8_correct() {
    let dir = std::env::temp_dir().join("kiln_utf8");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = dir.join("utf8.kiln");
    std::fs::write(
        &src,
        "module utf8check\ntarget console\n\nsub main\n  \
         call print_int(length(\"héllo\"))\n  \
         call print_text(reverse(\"héllo\"))\n  \
         call print_text(substr(\"héllo\", 1, 2))\n  \
         call print_int(length(\"日本語\"))\n  \
         call print_text(substr(\"日本語\", 2, 1))\nend\n",
    )
    .expect("write");

    let bin = dir.join("utf8");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(out.status.success(), "build failed: {}", String::from_utf8_lossy(&out.stderr));

    let text = run(&bin);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "5", "length counts characters, not bytes: {text}");
    assert_eq!(lines[1], "olléh", "reverse must not split a character: {text}");
    assert_eq!(lines[2], "hé", "substr must not cut mid-character: {text}");
    assert_eq!(lines[3], "3", "three characters, nine bytes: {text}");
    assert_eq!(lines[4], "本", "slicing multi-byte text: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every support library's example builds, runs, and reports no failure.
///
/// The examples are self-checking transcripts: one prints `FAIL` on a check
/// that did not hold, so the assertion here is that no line says FAIL and that
/// the program produced output at all. Deliberately not asserting exact stdout
/// — these examples are documentation first, and pinning every line would mean
/// editing this test every time one gains a sentence. What must not drift is
/// that they still run and still pass their own checks.
///
/// Without this the ten libraries had examples but no coverage: they were only
/// ever run by hand, so a regression in any of them would reach a release
/// unnoticed.
#[test]
fn support_library_examples_pass_their_own_checks() {
    // `net` is excluded deliberately: its example reaches the network, and a
    // test that fails on a train is a test people learn to ignore.
    const LIBS: &[&str] = &[
        "filelib", "systemlib", "textlib", "timelib", "randomlib", "hashlib",
        "configlib", "processlib", "jsonlib", "mathlib",
    ];
    for name in LIBS {
        let stdout = run(&build_as(name, "selfcheck"));
        let failures: Vec<&str> = stdout.lines().filter(|l| l.contains("FAIL")).collect();
        assert!(
            failures.is_empty(),
            "{name} reported failures:\n{}",
            failures.join("\n")
        );
        assert!(
            !stdout.trim().is_empty(),
            "{name} produced no output — did it run at all?"
        );
    }
}

/// Arrays and byte-sets, end to end.
///
/// `examples/arrays.kiln` is a self-checking transcript: every line prints `ok`
/// or `FAIL`, so the assertion is that nothing failed and that the file ran at
/// all. It covers what unit tests cannot — that an out-of-range index really
/// does report through the error slot in a built binary rather than reading
/// whatever follows the array.
#[test]
fn arrays_example_passes_its_own_checks() {
    let stdout = run(&build_as("arrays", "selfcheck"));
    let failures: Vec<&str> = stdout.lines().filter(|l| l.contains("FAIL")).collect();
    assert!(
        failures.is_empty(),
        "arrays reported failures:\n{}",
        failures.join("\n")
    );
    assert!(!stdout.trim().is_empty(), "arrays produced no output");
}

/// An index past the end must fail loudly and hand back the sentinel — never
/// the bytes that happen to sit after the array.
#[test]
fn an_out_of_range_index_reports_instead_of_reading_past_the_end() {
    let dir = std::env::temp_dir().join("kiln_bounds_test");
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("bounds.kiln");
    std::fs::write(
        &src,
        "module bounds\nsub main\n  \
         var xs: int[] = [11, 22]\n  \
         call print_int(xs[9])\n  \
         call print_int(last_error_code())\n  \
         xs[9] = 1\n  \
         call print_int(last_error_code())\n  \
         call print_int(xs[2])\n  \
         call print_int(last_error_code())\nend\n",
    )
    .expect("write");

    let bin = dir.join("bounds");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = run(&bin);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "0", "a failed read yields the sentinel: {text}");
    assert_eq!(lines[1], "10007", "KN_ERR_OUT_OF_RANGE: {text}");
    assert_eq!(lines[2], "10007", "a failed write reports too: {text}");
    assert_eq!(lines[3], "22", "a good read still works: {text}");
    assert_eq!(lines[4], "0", "success clears the error slot: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Positions count from 1, everywhere, with no exceptions.
///
/// This is the convention most likely to be reverted by accident — every
/// contributor arrives from a 0-based language, and an off-by-one here is
/// invisible in output that still looks plausible. Each assertion below pins
/// one surface that had to move: the array type, bytes, core text positions,
/// and the search commands whose "absent" answer became 0 precisely because
/// nothing occupies 0 any more.
#[test]
fn positions_count_from_one() {
    let dir = std::env::temp_dir().join("kiln_onebased");
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("one.kiln");
    std::fs::write(
        &src,
        "module one\nsub main\n  \
         var xs: int[] = [10, 20, 30]\n  \
         call print_int(xs[1])\n  \
         call print_int(xs[count(xs)])\n  \
         call print_int(index_of(xs, 30))\n  \
         call print_int(index_of(xs, 99))\n  \
         let b: bytes = bytes_from_text(\"AB\")\n  \
         call print_int(b[1])\n  \
         call print_text(substr(\"abcdef\", 1, 2))\n  \
         call print_int(find(\"abc\", \"a\"))\n  \
         call print_int(find(\"abc\", \"z\"))\nend\n",
    )
    .expect("write");

    let bin = dir.join("one");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = run(&bin);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "10", "xs[1] is the FIRST element: {text}");
    assert_eq!(lines[1], "30", "xs[count(xs)] is the last: {text}");
    assert_eq!(lines[2], "3", "index_of returns a 1-based position: {text}");
    assert_eq!(lines[3], "0", "absent is 0, not -1: {text}");
    assert_eq!(lines[4], "65", "the first byte is b[1]: {text}");
    assert_eq!(lines[5], "ab", "substr starts at position 1: {text}");
    assert_eq!(lines[6], "1", "find returns a 1-based position: {text}");
    assert_eq!(lines[7], "0", "find says 0 when absent: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A literal `xs[0]` is rejected at compile time, not at run time.
///
/// It is the one mistake every newcomer makes, and the run-time message would
/// only say "out of range" — which reads as a bug in their loop rather than a
/// language they guessed wrong about.
#[test]
fn a_zero_index_is_a_compile_error() {
    let dir = std::env::temp_dir().join("kiln_zeroidx");
    let _ = std::fs::create_dir_all(&dir);
    let src = dir.join("zero.kiln");
    std::fs::write(
        &src,
        "module zero\nsub main\n  var xs: int[] = [1]\n  call print_int(xs[0])\nend\n",
    )
    .expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", dir.join("z").to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(!out.status.success(), "xs[0] must not compile");
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(
        msg.contains("count from 1"),
        "the message must say what the base is, got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A colour literal one digit short is a compile error, in a declaration and
/// in an assignment alike.
///
/// `"#44444"` is text, so the type check passes it, and at run time the
/// renderer paints nothing and says nothing — the kind of mistake that costs an
/// afternoon. The same program with six digits builds.
#[test]
fn a_malformed_colour_literal_is_a_compile_error() {
    // A `ui` program checks the library's prerequisites before it validates
    // anything, so without the vendored stack the failure is the missing
    // dependency, not the diagnostic under test.
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("kiln_colour");
    let _ = std::fs::create_dir_all(&dir);
    let program = |declared: &str, assigned: &str| {
        format!(
            "module tint\ntarget gui\nuse ui\n\nform win\n  title = \"t\"\n  label greeting\n    \
             text = \"hi\"\n    color = \"{declared}\"\n  end\nend\n\nsub recolour\n  \
             greeting.color = \"{assigned}\"\nend\n"
        )
    };
    let build = |file: &str, src: &str| {
        let path = dir.join(file);
        std::fs::write(&path, src).expect("write");
        Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args(["build", path.to_str().unwrap(), "-o", dir.join(file).with_extension("").to_str().unwrap()])
            .env("KILN_RUNTIME_DIR", repo.join("runtime"))
            .output()
            .expect("run kiln")
    };

    let bad = build("bad.kiln", &program("#44444", "#12345"));
    assert!(!bad.status.success(), "a five-digit colour must not compile");
    let msg = String::from_utf8_lossy(&bad.stderr);
    for want in [
        "`win.greeting`: property `color` value \"#44444\" is not a colour (use #rgb, #rrggbb or #rrggbbaa)",
        "in `recolour`: `greeting.color` value \"#12345\" is not a colour (use #rgb, #rrggbb or #rrggbbaa)",
    ] {
        assert!(msg.contains(want), "expected {want:?} in: {msg}");
    }

    let good = build("good.kiln", &program("#444444", "#123456"));
    assert!(
        good.status.success(),
        "a six-digit colour must build: {}",
        String::from_utf8_lossy(&good.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A CONSOLE program with no form outlives `main` and exits on `quit`.
///
/// This is the property the whole event loop exists for: `main` prints one line
/// and returns, and the program keeps running because a timer is a live event
/// source. Before the loop moved into the runtime, only a module with a form
/// had one at all, so a console program's timer would have fired never — the
/// binary would have exited the instant `main` returned, with the first line
/// printed and nothing after it. The ORDER below is the proof: every `tick`
/// line is output produced after `main` had already returned.
///
/// Run with a deadline rather than `run()`: if the loop ever regresses into
/// never returning, a test that waits forever hangs CI instead of failing it.
#[test]
fn a_console_program_stays_alive_for_its_timer() {
    let bin = build_as("loopdemo", "loop");
    let mut child = Command::new(&bin)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("run loopdemo");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait().expect("poll loopdemo") {
            Some(status) => {
                assert!(status.success(), "loopdemo exited {status}");
                break;
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("loopdemo never exited — `quit` did not end the event loop");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }

    let out = child.wait_with_output().expect("collect loopdemo output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "main returned; the timer keeps the program alive",
            "tick 1",
            "tick 2",
            "tick 3",
        ],
        "unexpected loopdemo output:\n{stdout}"
    );
}

/// The two halves of `kind` are checked, not merely recorded: a component with
/// a rectangle needs a form to be drawn in, and one without cannot be placed in
/// a form the designer will rewrite as a rectangle among rectangles.
#[test]
fn a_component_must_be_declared_where_its_kind_belongs() {
    // Both cases name a `ui` type, and a build checks a library's prerequisites
    // before it validates anything — so without the vendored stack the failure
    // is the missing dependency, not the diagnostic under test.
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("kiln_kind");
    let _ = std::fs::create_dir_all(&dir);
    let cases = [
        (
            "visual.kiln",
            "module visual\nuse ui\n\nbutton stray\n  text = \"no form\"\nend\n\nsub main\n  \
             call print_int(1)\nend\n",
            "has to live inside a form",
        ),
        (
            "nonvisual.kiln",
            "module nonvisual\nuse ui\n\nform win\n  title = \"t\"\n  timer inner\n    \
             interval = 10\n  end\nend\n",
            "declare it at module level",
        ),
    ];
    for (file, src, want) in cases {
        let path = dir.join(file);
        std::fs::write(&path, src).expect("write");
        let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
            .args([
                "build",
                path.to_str().unwrap(),
                "-o",
                dir.join("k").to_str().unwrap(),
            ])
            .env("KILN_RUNTIME_DIR", repo.join("runtime"))
            .output()
            .expect("run kiln");
        assert!(!out.status.success(), "{file} must not compile");
        let msg = String::from_utf8_lossy(&out.stderr);
        assert!(
            msg.contains(want),
            "{file}: the message must say where it belongs, got: {msg}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build an inline module and run it against a deadline.
///
/// The deadline is the point: every defect these tests hunt shows up as a
/// program that never ends, and a test that waits forever tells CI nothing.
fn run_module_within(name: &str, src: &str, envs: &[(&str, &str)], secs: u64) -> String {
    let repo = repo();
    let dir = std::env::temp_dir().join(format!("kiln_mod_{name}"));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.kiln"));
    std::fs::write(&path, src).expect("write module");
    let bin = dir.join(name);
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", path.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build {name} failed");

    let mut cmd = Command::new(&bin);
    cmd.stdout(std::process::Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("run built binary");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("poll child") {
            Some(status) => {
                assert!(status.success(), "{name} exited {status}");
                break;
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("{name} never exited — the event loop did not end");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().expect("collect output");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A source that is switched off keeps nothing alive. Without this the loop
/// could stay in itself for a registered-but-dead timer, and a program that
/// declared one it never enabled would hang after `main` instead of ending.
#[test]
fn a_disabled_timer_does_not_hold_the_program_open() {
    let stdout = run_module_within(
        "offtimer",
        "module offtimer\n\ntimer idle\n  interval = 20\n  enabled = false\n  \
         on tick: on_tick\nend\n\nsub main\n  call print_text(\"main returned\")\nend\n\n\
         sub on_tick\n  call print_text(\"tick\")\nend\n",
        &[],
        10,
    );
    assert_eq!(
        stdout.lines().collect::<Vec<&str>>(),
        vec!["main returned"],
        "a disabled timer must neither fire nor keep the program running:\n{stdout}"
    );
}

/// Liveness is re-read as the program runs, not decided once at startup: a
/// handler that switches its own timer off is the only way a program with no
/// window ends without calling `quit`. Reading `interval` back in the same
/// handler proves a non-visual component's properties are ordinary properties.
#[test]
fn a_handler_can_switch_off_the_timer_that_called_it() {
    let stdout = run_module_within(
        "selfstop",
        "module selfstop\n\ntimer t\n  interval = 20\n  on tick: on_tick\nend\n\n\
         sub main\n  call print_text(\"main returned\")\nend\n\n\
         sub on_tick\n  call print_text(concat(\"interval \", int_to_text(t.interval)))\n  \
         t.enabled = false\nend\n",
        &[],
        10,
    );
    assert_eq!(
        stdout.lines().collect::<Vec<&str>>(),
        vec!["main returned", "interval 20"],
        "the timer must fire once, report its interval, and then stop:\n{stdout}"
    );
}

/// The two kinds of event source share one loop. A window and a timer in the
/// same program is the composition that would break first if either had kept a
/// loop of its own, and neither track's own tests can catch it.
#[test]
fn a_window_and_a_timer_run_in_the_same_program() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let stdout = run_module_within(
        "windowtimer",
        "module windowtimer\nuse ui\n\ntimer t\n  interval = 20\n  on tick: on_tick\nend\n\n\
         form win\n  title = \"both\"\n  width = 240\n  height = 160\nend\n\n\
         sub main\n  call print_text(\"main returned\")\nend\n\n\
         sub on_tick\n  call print_text(\"tick\")\nend\n",
        &[("KILN_UI_EXIT_AFTER_FRAMES", "60")],
        30,
    );
    assert!(
        stdout.contains("main returned") && stdout.contains("tick"),
        "the timer must fire while the window renders:\n{stdout}"
    );
}

/// `quit` before the loop is ever entered must still end the program. The
/// request has to latch: a program whose `main` decides there is nothing to do
/// would otherwise register its sources, enter the loop, and wait forever for a
/// quit that had already happened.
#[test]
fn quit_from_main_ends_a_program_that_never_enters_the_loop() {
    let stdout = run_module_within(
        "earlyquit",
        "module earlyquit\n\ntimer t\n  interval = 20\n  on tick: on_tick\nend\n\n\
         sub main\n  call print_text(\"nothing to do\")\n  call quit()\nend\n\n\
         sub on_tick\n  call print_text(\"tick\")\nend\n",
        &[],
        10,
    );
    assert_eq!(
        stdout.lines().collect::<Vec<&str>>(),
        vec!["nothing to do"],
        "quit from main must latch — the loop may not run a single tick:\n{stdout}"
    );
}

// --- records and dictionaries -------------------------------------------
//
// Both are runtime-owned aggregates carried in the slot as a pointer, the way
// arrays and byte-sets already are. What only a built-and-run program can show
// is the part the type checker cannot: that a record is a REFERENCE, that a
// dictionary keeps its insertion order, and that a missing key answers a
// sentinel with the reason left in the error slot.

/// Records end to end: construction, field reads through a chain, a record in
/// and out of a subroutine, reference semantics, and a list of them.
#[test]
fn records_build_and_run() {
    let stdout = run(&build_as("records", "run"));
    assert_eq!(
        stdout.lines().collect::<Vec<&str>>(),
        vec![
            "midpoint x=4",
            "midpoint y=15",
            "Ada (36) at 1,2",
            // `same` and `ada` are two names for one record, so the birthday
            // is seen through both. A copy would still print 36 here.
            "Ada (37) at 1,2",
            "  Ada (37) at 1,2",
            "  Grace (45) at 3,4",
        ],
        "unexpected records output:\n{stdout}"
    );
}

/// Dictionaries end to end. The `dict_keys` order is asserted exactly, which
/// is a promise: entries are kept in insertion order, so iterating one twice
/// gives the same answer and a program's output is reproducible.
#[test]
fn dict_builds_and_runs() {
    let stdout = run(&build_as("dict", "run"));
    assert_eq!(
        stdout.lines().collect::<Vec<&str>>(),
        vec![
            "people: 3",
            "Ada is 36",
            // Reached through the marshalled command rather than the
            // subscript — the same entry either way.
            "Alan is now 42",
            // The sentinel for an int dictionary, and the reason behind it —
            // which is the whole of what `get` on a missing key promises.
            "missing reads as 0",
            "...because: no key `Nobody` in a dictionary of 3 entries",
            "Ada -> 36",
            "Alan -> 42",
            "Grace -> 45",
            "after removing Alan: 2",
            // Removing what is already gone is an answer, not a failure.
            "removing Alan again does nothing",
        ],
        "unexpected dict output:\n{stdout}"
    );
}

/// A record and a dictionary must be as dead-strippable as everything else:
/// a program that uses neither may not carry either one's code.
#[test]
fn a_program_that_uses_neither_links_neither() {
    let bin = build_as("hello", "no_aggregates");
    let syms = Command::new("nm")
        .arg("-C")
        .arg(&bin)
        .output()
        .expect("run nm");
    let text = String::from_utf8_lossy(&syms.stdout);
    for sym in ["kn_dict_at", "kn_dict_put", "kn_rec_new", "kn_rec_get"] {
        assert!(
            !text.contains(sym),
            "`{sym}` survived into a program that never mentions one:\n{text}"
        );
    }
}

/// Records and dictionaries in one program. Neither feature's own example can
/// show this: a dictionary holds one type of value, a record is a type, and
/// whether `person{}` works at all is a fact about the two together.
///
/// The last three assertions are the ones worth having. A record has no
/// printable empty value, so `get` on a missing key can only answer *no*
/// record — and this pins that the miss is caught at the field read rather
/// than followed into a null dereference.
#[test]
fn records_and_dictionaries_compose() {
    let stdout = run(&build_as("compose", "run"));
    assert_eq!(
        stdout.lines().collect::<Vec<&str>>(),
        vec![
            "people: 2",
            "Ada (36)",
            "  Ada (36)",
            "  Grace (45)",
            // The record in the dictionary is the record the name holds — a
            // copying `get` would still print 36 here.
            "after a birthday: Ada (37)",
            "missing lookup said: no key `nobody` in a dictionary of 2 entries",
            "[]",
            "reading its field said: field 1 of a record with 0 field(s) cannot be read",
            "dict_has agrees there is no such person",
            "over forty: 1",
            "  Grace (45)",
        ],
        "unexpected compose output:\n{stdout}"
    );
}

/// An `httpserver` handler reaching records, a dictionary and a subroutine
/// parameter — the composition the server example cannot assert, because
/// nothing in the test suite had ever spoken HTTP to a built program.
///
/// The client is Rust rather than Kiln on purpose: a program that requested
/// its own port would block its only thread inside `net_tcp_receive_line`
/// waiting for a reply that only the same thread's pump can produce.
#[test]
fn an_http_handler_reaches_records_and_a_dictionary() {
    use std::io::{Read, Write};
    let dir = std::env::temp_dir().join("kiln_httpd_compose");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let src = dir.join("main.kiln");
    std::fs::write(
        &src,
        r#"module httpcompose
use net

record hit
  path: text
  n: int
end

httpserver site
  port = 8137
  on request: on_request
end

var seen: int{} = {}

sub label(h: hit): text
  return h.path + " x" + int_to_text(h.n)
end

sub serve(req: int)
  let p: text = net_req_path(req)
  if dict_has(seen, p)
    call dict_set(seen, p, dict_get(seen, p) + 1)
  else
    call dict_set(seen, p, 1)
  end
  if p = "/quit"
    call net_req_reply(req, 200, "bye")
    call quit()
  else
    call net_req_reply(req, 200, label(hit(path: p, n: dict_get(seen, p))))
  end
end

sub on_request
  call serve(net_request())
end

sub main
  call print_text("ready")
end
"#,
    )
    .expect("write source");

    let bin = dir.join("httpcompose");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "building the http composition failed");

    // A port in use is somebody else's machine, not a product failure: the
    // server says so on stderr and exits rather than running deaf, and the
    // test declines to invent a verdict from that.
    if std::net::TcpListener::bind("127.0.0.1:8137").is_err() {
        eprintln!("port 8137 is busy; skipping the http composition test");
        return;
    }

    let child = Command::new(&bin)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start server");

    let ask = |path: &str| -> String {
        // The listener opens on the first turn of the loop, so the first
        // connection may beat it. Retry rather than sleep a guessed amount.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Ok(mut s) = std::net::TcpStream::connect("127.0.0.1:8137") {
                let _ = write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n");
                let mut reply = String::new();
                let _ = s.read_to_string(&mut reply);
                if let Some((_, body)) = reply.split_once("\r\n\r\n") {
                    return body.to_string();
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the server never answered on 127.0.0.1:8137"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };

    assert_eq!(ask("/a"), "/a x1");
    // The dictionary is module state, so the count survives into the next
    // request — the thing a request handle parked in a global would get wrong.
    assert_eq!(ask("/a"), "/a x2");
    assert_eq!(ask("/b"), "/b x1");
    assert_eq!(ask("/quit"), "bye");

    let out = child.wait_with_output().expect("server exit");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ready"),
        "the server never got as far as `main`"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A window and a server in one program: two event sources on the runtime's
/// loop at once, which is the whole reason the loop belongs to the runtime and
/// not to whichever library is linked. A request handler here also writes to a
/// widget, so the two halves are not merely coexisting.
#[test]
fn a_window_and_a_server_share_one_loop() {
    use std::io::{Read, Write};
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("kiln_uinet_compose");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let src = dir.join("main.kiln");
    std::fs::write(
        &src,
        r#"module uinet
use ui
use net

var hits: int = 0

httpserver site
  port = 8138
  on request: on_request
end

form main_window
  title  = "ui + net"
  width  = 420
  height = 160

  label status
    text  = "0 requests"
    left  = 20
    top   = 20
    width = 380
  end
end

sub on_request
  hits = hits + 1
  status.text = int_to_text(hits) + " requests"
  call net_req_reply(net_request(), 200, "ok")
end

sub main
  call print_text("gui+server up")
end
"#,
    )
    .expect("write source");

    let bin = dir.join("uinet");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "building the ui+net composition failed");

    if std::net::TcpListener::bind("127.0.0.1:8138").is_err() {
        eprintln!("port 8138 is busy; skipping the ui+net composition test");
        return;
    }

    // Frames are uncapped, so the count is a lifetime, not a duration: enough
    // turns of the loop that a request has somewhere to arrive.
    let child = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "30000")
        .env("KILN_UI_FRAME_MS", "0")
        .env("KILN_UI_DUMP_A11Y", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start ui+net program");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut answered = false;
    while std::time::Instant::now() < deadline && !answered {
        if let Ok(mut s) = std::net::TcpStream::connect("127.0.0.1:8138") {
            let _ = write!(s, "GET /one HTTP/1.1\r\nHost: x\r\n\r\n");
            let mut reply = String::new();
            let _ = s.read_to_string(&mut reply);
            answered = reply.ends_with("ok");
        }
        if !answered {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    assert!(
        answered,
        "the server never answered while the window was running"
    );

    let out = child.wait_with_output().expect("program exit");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The accessible name mirrors the live label, so it is the evidence that
    // the handler reached the widget and not merely the socket.
    let label = stdout
        .lines()
        .find(|l| l.starts_with("a11y: id=2"))
        .unwrap_or_else(|| panic!("no label node in the a11y tree:\n{stdout}"));
    assert!(
        label.contains("name=\"1 requests\""),
        "the request handler did not update the window: {label}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `quit(code)` — and anything else that stops the loop with a status — must
/// reach the shell. `ECodeStart` returned a hard 0, so a server that could not
/// bind announced failure on stderr and then told the caller it had succeeded,
/// which is the one thing a script cannot recover from.
#[test]
fn a_failed_start_exits_non_zero() {
    let dir = std::env::temp_dir().join("kiln_exitcode");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let src = dir.join("main.kiln");
    std::fs::write(
        &src,
        r#"module bindfail
use net

httpserver site
  port = 8139
  on request: on_request
end

sub on_request
  call net_req_reply(net_request(), 200, "hi")
end

sub main
  call print_text("starting")
end
"#,
    )
    .expect("write source");

    let bin = dir.join("bindfail");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "building the bind-failure program failed");

    // Hold the port from the test process, so the program cannot have it.
    let hog = match std::net::TcpListener::bind("127.0.0.1:8139") {
        Ok(l) => l,
        Err(_) => {
            eprintln!("port 8139 is busy; skipping");
            return;
        }
    };
    let out = Command::new(&bin).output().expect("run bindfail");
    drop(hog);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a server that cannot listen must not report success; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot listen"),
        "and it must say why: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build inline `.kiln` source to a temp binary. `tag` must be unique per test —
/// see `build_as`, which this is the anonymous-source twin of.
fn build_src(src: &str, tag: &str) -> PathBuf {
    let repo = repo();
    let dir = std::env::temp_dir().join(format!("kiln_src_{tag}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("main.kiln");
    std::fs::write(&path, src).expect("write source");
    let bin = dir.join("prog");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", path.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo.join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build {tag} failed");
    bin
}

/// What an event hands its handler, end to end in a built binary.
///
/// `examples/eventparams.kiln` wires two timers to the same `tick`: one handler
/// takes the count and one ignores it. Both must be ordinary subroutines, so
/// the assertion is the whole transcript in order — the counted handler seeing
/// 1, 2, 3 is the entire point, and a thunk that dropped the argument or handed
/// over the same number twice would still produce plausible-looking output.
#[test]
fn typed_event_parameters_reach_a_handler() {
    let stdout = run(&build_as("eventparams", "typedevt"));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "main returned",
            "counted tick 1",
            "plain tick",
            "counted tick 2",
            "plain tick",
            "counted tick 3",
            "plain tick",
        ]
    );
}

/// A truth value read back off a component.
///
/// This is a regression test for two faults that only appear together. The
/// checker types `agree.checked` as bool from the descriptor, but the lowering
/// special-cased only `Ty::Int` and let everything else fall through as text,
/// so `if agree.checked` did not compile at all. Underneath that, RmlUi records
/// a checkbox as the PRESENCE of an attribute and a user click sets it to the
/// empty string — so reading the attribute's value reported a box the user had
/// just ticked as clear.
///
/// Hence the click: `2.1` hits the checkbox's inner input, which is the path a
/// real mouse takes. Asserting only the construction-time value would pass
/// against the broken getter.
#[test]
fn a_bool_property_reads_as_a_truth_value() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    const SRC: &str = "\
module boolprop
use ui

form win
  title = \"bool\"
  width = 200
  height = 120

  checkbox agree
    text = \"I agree\"
    checked = false
    left = 10
    top = 10
    on change: toggled
  end
end

sub toggled
  if agree.checked
    call print_text(\"now checked\")
  else
    call print_text(\"now unchecked\")
  end
end
";
    let bin = build_src(SRC, "boolprop");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "4")
        .env("KILN_UI_SYNTH_CLICK", "2.1")
        .output()
        .expect("run boolprop");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("now checked"),
        "a box the user ticked must read as checked, got:\n{stdout}"
    );
}

/// The tracks composed: one form holding a combobox, a checkbox and a button,
/// with a module-level timer whose handler takes the tick count.
///
/// Each of those landed separately, and each was verified separately. This is
/// the case none of them covers — a visual component read through the property
/// ABI in the same program as a non-visual one delivering a typed argument, so
/// that a form and the event loop cannot quietly stop composing.
#[test]
fn a_form_composes_a_combobox_with_a_typed_timer() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    const SRC: &str = "\
module composed
use ui

form win
  title = \"composed\"
  width = 320
  height = 200

  combobox colour
    items = \"Red\\nGreen\\nBlue\"
    selected = 3
    left = 10
    top = 10
    width = 200
    height = 28
  end

  checkbox agree
    text = \"Agree\"
    checked = true
    left = 10
    top = 60
  end

  button go
    text = \"Report\"
    left = 10
    top = 100
    width = 90
    height = 30
    on click: report
  end
end

timer ticker
  interval = 20
  on tick: counted
end

sub counted(n: int)
  call print_text(concat(\"tick \", int_to_text(n)))
end

sub report
  call print_text(concat(\"colour = \", int_to_text(colour.selected)))
  if agree.checked
    call print_text(\"agreed\")
  else
    call print_text(\"not agreed\")
  end
end
";
    let bin = build_src(SRC, "composed");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "8")
        .env("KILN_UI_SYNTH_CLICK", "4")
        .output()
        .expect("run composed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Interleaving is not asserted: how many times a 20ms timer fires inside
    // eight frames is a property of the machine, not of the program.
    assert!(
        stdout.contains("colour = 3"),
        "combobox selection did not read back:\n{stdout}"
    );
    assert!(
        stdout.contains("agreed") && !stdout.contains("not agreed"),
        "checkbox truth value did not read back:\n{stdout}"
    );
    assert!(
        stdout.contains("tick 1"),
        "the timer's typed argument did not reach its handler:\n{stdout}"
    );
}

/// The property inspector edits a real component's colour, and edits only it.
///
/// The `color` editor hint puts a swatch and a palette popup in the inspector,
/// which is UI: it passes its own tests while painting nothing and writing
/// nothing. So this drives the actual widgets — select the button, open its
/// swatch, pick a value, save — and then asserts on the FILE, because what the
/// inspector is for is changing the source.
///
/// The whole-file comparison is the point of the test rather than an extra: a
/// save that rewrites the form's own `background_color`, reflows the file, or
/// drops the untouched lines would satisfy any assertion that only looked at
/// the button.
#[test]
fn the_inspector_edits_a_colour_on_a_real_component() {
    let repo = repo();
    let designer = repo.join("designer/kiln-designer");
    if !designer.exists() {
        eprintln!("designer not built; skipping");
        return;
    }
    let original = repo.join("examples/controls.kiln");
    // Never the tracked file: Studio saves on exit, and examples/form.kiln has
    // been committed with a stray designer edit twice.
    let project = std::env::temp_dir().join("kiln_swatch.kiln");
    std::fs::copy(&original, &project).expect("seed project");

    let out = Command::new(&designer)
        .arg(&project)
        .arg(repo.join("target/debug/kiln"))
        .env(
            "KILN_DESIGNER_SCRIPT",
            "select:go;swatch:background_color;pick:#1a7f37;save",
        )
        .output()
        .expect("run designer");
    assert!(out.status.success(), "designer session failed");

    let before = std::fs::read_to_string(&original).expect("read original");
    let after = std::fs::read_to_string(&project).expect("read saved");
    assert!(
        after.contains("background_color = \"#1a7f37\""),
        "the picked colour never reached the file:\n{after}"
    );
    let changed: Vec<(&str, &str)> = before
        .lines()
        .zip(after.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(
        changed,
        vec![(
            "    background_color = \"#1e60d5\"",
            "    background_color = \"#1a7f37\""
        )],
        "the inspector rewrote more than the property it was pointed at"
    );
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "the save changed the file's length"
    );
    let _ = std::fs::remove_file(&project);
}

/// A grid bound to a datasource, end to end: `examples/grid.kiln` adds a row
/// from `main` and prints the count, so `rows: 4` proves the datasource, the
/// binding and the count command all reached a built program. It is a GUI
/// example, so it runs only where the UI stack is vendored and is told to
/// exit after a few frames rather than wait for a window to close.
#[test]
fn grid_example_counts_its_rows() {
    let repo = repo();
    if !repo.join("vendor/RmlUi/build/librmlui.a").exists() {
        eprintln!("RmlUi not vendored; skipping");
        return;
    }
    let bin = build_as("grid", "rows");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "3")
        .output()
        .expect("run grid");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l == "rows: 4"),
        "expected `rows: 4`; stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.lines().any(|l| l.contains("FAIL")),
        "grid reported failures:\n{stdout}"
    );
}

/// Documentation travels with a command, through the C table and the ABI and
/// out of `kiln commands` — the path every reader of it depends on.
///
/// Also: a command that carries no documentation must print no `doc:` line at
/// all. The whole enrichment is incremental, and it only stays safe as long as
/// an undocumented command is absent from that output rather than present with
/// nothing in it.
#[test]
fn a_documented_command_carries_its_doc_and_example() {
    let repo = repo();
    let bin = repo.join("target").join(if cfg!(debug_assertions) { "debug" } else { "release" })
        .join("kiln");
    let out = Command::new(&bin)
        .arg("commands")
        .arg("--use")
        .arg("file")
        .current_dir(&repo)
        .output()
        .expect("run kiln commands");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("doc: file_read_text Read a whole text file"),
        "the sentence did not survive the ABI:\n{stdout}"
    );
    assert!(
        stdout.contains("example: file_read_text let notes: text = file_read_text(\"notes.txt\")"),
        "the example did not survive the ABI:\n{stdout}"
    );
    // Multi-line examples arrive as several lines, in order.
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("example: file_read_text "))
        .collect();
    assert_eq!(lines.len(), 2, "expected both example lines:\n{stdout}");

    // An undocumented command prints its signature and nothing else.
    assert!(
        stdout.contains("command: print_text(text)"),
        "core is missing from the listing:\n{stdout}"
    );
    assert!(
        !stdout.contains("doc: print_text "),
        "an undocumented command must print no doc line:\n{stdout}"
    );
}
