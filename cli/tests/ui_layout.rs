//! Layout and validation in the `ui` library, end to end in built binaries:
//! anchors that follow a resize, colours refused before the substrate sees
//! them, and where a form's window opens.
//!
//! Everything runs headless through the UI test hooks (abi/kiln_ui.h):
//! `KILN_UI_SIZE` stands in for a window manager's resize, and
//! `KILN_UI_DEBUG` prints the rectangle each anchored control was moved
//! to — by widget handle, because component identifiers never reach the
//! binary (G8). The form is handle 1 and its children follow in declaration
//! order.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The GUI stack is vendored separately; without it there is nothing to test.
fn ui_available() -> bool {
    if repo().join("vendor/RmlUi/build/librmlui.a").exists() {
        return true;
    }
    eprintln!("RmlUi not vendored (run tools/fetch-rmlui.sh); skipping GUI test");
    false
}

/// Build `<repo>/examples/<name>.kiln`. `tag` must be unique per test: tests
/// run in parallel, and two writing one output path race each other.
fn build_as(name: &str, tag: &str) -> PathBuf {
    let example = repo().join("examples").join(format!("{name}.kiln"));
    build_file(&example, &std::env::temp_dir().join(format!("kiln_{name}_{tag}_layout")))
}

/// Build inline source, for the cases small enough not to deserve an example.
fn build_src(src: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_layout_{tag}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("main.kiln");
    std::fs::write(&path, src).expect("write source");
    build_file(&path, &dir.join("prog"))
}

fn build_file(source: &Path, out_bin: &Path) -> PathBuf {
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", source.to_str().unwrap(), "-o", out_bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build {} failed", source.display());
    out_bin.to_path_buf()
}

/// Run a GUI binary headless for `frames` frames, resized to `size` when one
/// is given, with the anchor debug lines on.
fn run_headless(bin: &Path, frames: &str, size: Option<&str>) -> Output {
    let mut c = Command::new(bin);
    c.env("KILN_UI_EXIT_AFTER_FRAMES", frames)
        .env("KILN_UI_DEBUG", "1");
    if let Some(s) = size {
        c.env("KILN_UI_SIZE", s);
    }
    let out = c.output().expect("run built binary");
    assert!(
        out.status.success(),
        "binary exited non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn has_line(out: &str, line: &str) -> bool {
    out.lines().any(|l| l == line)
}

/// The form is 400x300 and the window becomes 600x450, so every anchored
/// control sees a delta of 200,150. The button (handle 2) is anchored
/// right,bottom and moves by the whole delta; the editbox (handle 3) is
/// anchored left,right and grows by the width only; the label has the default
/// anchors and is not reported at all — nothing moved it.
#[test]
fn anchored_controls_follow_a_resize() {
    if !ui_available() {
        return;
    }
    let bin = build_as("anchors", "resize");
    let out = run_headless(&bin, "8", Some("600x450"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        has_line(&stderr, "ui: anchored 2 -> 450,380 120x36"),
        "button did not follow the bottom-right corner:\n{stderr}"
    );
    assert!(
        has_line(&stderr, "ui: anchored 3 -> 20,44 560x26"),
        "editbox did not stretch with the width:\n{stderr}"
    );
    assert!(
        !stderr.contains("ui: anchored 4 "),
        "a control with the default anchors was moved:\n{stderr}"
    );
}

/// A control moved or re-anchored from a subroutine must not snap back on the
/// next resize: `left = 300` written before the window is up is the new base,
/// and `anchors = "right"` keeps only the sideways movement. A misspelled edge
/// is refused with an error the program can read, and the anchors it had stay.
#[test]
fn geometry_set_at_run_time_becomes_the_anchor_base() {
    if !ui_available() {
        return;
    }
    let src = r#"module rebase
use ui
form win
  title = "rebase"
  width = 400
  height = 300
  button ok
    text = "OK"
    left = 250
    top = 230
    width = 120
    height = 36
    anchors = "right,bottom"
  end
end
sub main
  ok.left = 300
  ok.anchors = "right"
  ok.anchors = "middle"
  call print_text(int_to_text(last_error_code()))
  call print_text(last_error_text())
  call print_text(ok.anchors)
end
"#;
    let bin = build_src(src, "rebase");
    let out = run_headless(&bin, "4", Some("600x450"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.first().copied(), Some("10005"), "bad anchors set no error:\n{stdout}");
    assert!(
        lines.get(1).map_or(false, |l| l.starts_with("anchors:")),
        "error text does not name the property:\n{stdout}"
    );
    assert_eq!(lines.get(2).copied(), Some("right"), "bad anchors replaced good ones:\n{stdout}");
    assert!(
        has_line(&stderr, "ui: anchored 2 -> 500,230 120x36"),
        "the run-time left/anchors were not the base of the resize:\n{stderr}"
    );
}

/// `#44444` is not a colour. The substrate's answer would be a syntax error
/// on stderr and a set that reports success; the library's is an error the
/// program can read, and nothing on stderr.
#[test]
fn a_bad_colour_is_refused_before_the_substrate_sees_it() {
    if !ui_available() {
        return;
    }
    let bin = build_as("badcolour", "refuse");
    let out = run_headless(&bin, "2", None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "color: '#44444' is not a colour (#rgb, #rrggbb or #rrggbbaa)",
            "10005",
            "#4a86e8",
        ],
        "unexpected transcript:\n{stdout}"
    );
    assert!(
        !stderr.contains("Syntax error"),
        "the substrate still saw the bad colour:\n{stderr}"
    );
}

/// Where the window opens is three form properties. They must be declared —
/// the designer's inspector and the reference are built from this listing —
/// and a `manual` form must run headless and place its window from all
/// three whatever order it wrote them in. The window's actual coordinates are
/// deliberately not asserted: under Wayland a client cannot place its own
/// window, and a test of that would only pass on some desktops. (A form is
/// not addressable from a subroutine, so the read-back is the debug line.)
#[test]
fn a_form_declares_where_its_window_opens() {
    if !ui_available() {
        return;
    }
    let listing = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "ui"])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln commands");
    let listing = String::from_utf8_lossy(&listing.stdout);
    for line in [
        "property: form position text",
        "property: form left int",
        "property: form top int",
        "property: button anchors text",
        "editor: button anchors anchors",
        "property: grid anchors text",
    ] {
        assert!(has_line(&listing, line), "missing `{line}` in:\n{listing}");
    }

    let src = r#"module placed
use ui
form win
  title = "placed"
  width = 320
  height = 200
  left = 40
  top = 60
  position = "manual"
end
"#;
    let bin = build_src(src, "placed");
    let out = run_headless(&bin, "2", None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        has_line(&stderr, "ui: window position manual 40,60"),
        "the window was not placed from the form:\n{stderr}"
    );
}

/// The form's `load` runs once, before the first frame is drawn — so a handler
/// that fills in a caption is never seen half-done — and after the window
/// exists, so one that reads the form's size gets the real one. RmlUi raises no
/// such event; the library holds the handler and calls it from `kn_ui_run`.
#[test]
fn a_form_load_handler_runs_before_the_first_frame() {
    if !ui_available() {
        return;
    }
    let src = r#"module loadev
target gui
use ui
form win
  title = "load"
  width = 320
  height = 200
  on load: win_load
  label greeting
    text = "before"
    left = 20
    top = 20
    width = 200
    height = 24
  end
end
sub win_load
  call print_text("loaded")
  greeting.text = "after"
end
sub main
  call print_text("main")
end
"#;
    let bin = build_src(src, "loadev");
    let out = run_headless(&bin, "3", None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        has_line(&stdout, "loaded"),
        "the form's load handler never ran:\n{stdout}"
    );
    // `main` builds the form, so load can only come after it — and exactly once,
    // however many frames the loop turns.
    let order: Vec<&str> = stdout.lines().filter(|l| *l == "main" || *l == "loaded").collect();
    assert_eq!(order, vec!["main", "loaded"], "load fired at the wrong time:\n{stdout}");
    // What the handler wrote is what the first frame shows.
    assert!(
        String::from_utf8_lossy(&out.stderr).lines().all(|l| !l.contains("Syntax error")),
        "the handler's property write was refused:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
