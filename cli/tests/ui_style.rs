//! The control stylesheet, asserted in pixels.
//!
//! `libs/ui/ui_mapping.h` is the one place a control's rest appearance is
//! written, and it is shared by the runtime and by Studio's canvas — so a
//! change to it moves both. A unit test cannot see any of that: RCSS parses
//! silently past what it does not understand, a rule can be outranked by an
//! inline property, and a colour that never reaches the screen still reads
//! fine in the source. The only honest check is the frame.
//!
//! So every assertion here is a pixel from a built binary's own dump
//! (`KILN_UI_DUMP`, headless via `KILN_UI_EXIT_AFTER_FRAMES`), compared
//! with the token the specification pins for that surface. Points are chosen
//! mid-edge and away from text and corners: a rounded corner is antialiased
//! and a glyph is whatever the loaded face draws.
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// A frame from a built program: width, height and RGB triples.
struct Frame {
    width: usize,
    height: usize,
    px: Vec<u8>,
}

impl Frame {
    fn at(&self, x: usize, y: usize) -> String {
        assert!(
            x < self.width && y < self.height,
            "({x},{y}) is outside the {}x{} frame",
            self.width,
            self.height
        );
        let i = (y * self.width + x) * 3;
        format!("#{:02x}{:02x}{:02x}", self.px[i], self.px[i + 1], self.px[i + 2])
    }

    /// One pixel against the token the specification pins for that surface.
    fn expect(&self, x: usize, y: usize, colour: &str, what: &str) {
        assert_eq!(self.at(x, y), colour, "{what} at ({x},{y})");
    }
}

/// Parse the binary P6 the runtime dumps: `P6\n<w> <h>\n255\n` then RGB bytes.
fn read_ppm(path: &Path) -> Frame {
    let bytes = std::fs::read(path).expect("read dump");
    let mut fields = Vec::new();
    let mut at = 0;
    // Three whitespace-separated fields after the magic: width, height, max.
    while fields.len() < 4 && at < bytes.len() {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    at += 1; // the single whitespace byte that ends the header
    assert_eq!(fields[0], "P6", "not a binary PPM: {fields:?}");
    let width: usize = fields[1].parse().expect("width");
    let height: usize = fields[2].parse().expect("height");
    let px = bytes[at..].to_vec();
    assert_eq!(px.len(), width * height * 3, "short pixel data");
    Frame { width, height, px }
}

/// Build inline source, run it headless for four frames, and hand back the
/// frame it painted plus everything it said on stderr.
///
/// `tag` must be unique per test: tests run in parallel and two writing one
/// path race each other.
fn render(src: &str, tag: &str) -> (Frame, String) {
    let dir = std::env::temp_dir().join(format!("kiln_style_{tag}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("main.kiln");
    std::fs::write(&source, src).expect("write source");
    let bin = dir.join("prog");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build", source.to_str().unwrap(), "-o", bin.to_str().unwrap()])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build failed for {tag}");

    let dump = dir.join("frame.ppm");
    let out = Command::new(&bin)
        .env("KILN_UI_EXIT_AFTER_FRAMES", "4")
        .env("KILN_UI_DUMP", &dump)
        .output()
        .expect("run built binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "binary exited non-zero\nstderr:\n{stderr}");
    (read_ppm(&dump), stderr)
}

/// One form holding a button, an editbox, a checkbox, a combobox and a group
/// box, none of them naming a colour — so what appears is the stylesheet's
/// answer and nothing else.
const FORM: &str = r#"module styled
use ui
form win
  title = "styled"
  width = 400
  height = 300

  button ok
    text = "OK"
    left = 20
    top = 20
    width = 96
    height = 32
  end

  editbox name
    text = "Ada"
    left = 20
    top = 70
    width = 200
    height = 32
  end

  checkbox agree
    text = "I agree"
    checked = true
    left = 20
    top = 120
    width = 160
    height = 24
  end

  combobox pick
    items = "Red\nGreen\nBlue"
    selected = 1
    left = 20
    top = 160
    width = 200
    height = 32
  end

  groupbox opts
    text = "Options"
    left = 240
    top = 20
    width = 140
    height = 120
  end
end
"#;

/// The rest state of every control the specification pins a colour for, read
/// off the frame a built program painted.
#[test]
fn controls_wear_the_specified_rest_colours() {
    if !ui_available() {
        return;
    }
    let (f, _) = render(FORM, "rest");
    assert_eq!((f.width, f.height), (400, 300), "the dump is not the form's size");

    // The window's own ground: surface.canvas, not the old #f0f0f0.
    f.expect(2, 2, "#f3f3f3", "the form's ground");

    // A button that named no colour is the neutral one: white plate, one
    // hairline of border.control round it. (The accent variant has no
    // property to ask for it yet — a button that DOES name a fill keeps it.)
    f.expect(30, 36, "#ffffff", "the button's plate");
    f.expect(20, 36, "#d1d1d1", "the button's left outline");
    f.expect(30, 20, "#d1d1d1", "the button's top outline");

    // A combo box is a button-sized control with the same outline.
    f.expect(100, 175, "#ffffff", "the combo box's plate");
    f.expect(20, 175, "#d1d1d1", "the combo box's left outline");

    // A group box is a card: white, hairline border.default, 8px corners.
    f.expect(300, 80, "#ffffff", "the group box's ground");
    f.expect(240, 80, "#e5e5e5", "the group box's left border");
    f.expect(300, 20, "#e5e5e5", "the group box's top border");
}

/// A text input's bottom edge is DARKER than its other three — the one detail
/// that makes a Fluent field read as something you type into rather than a
/// panel. It is easy to lose (a later `border:` shorthand wipes it) and
/// invisible to every test but this one.
#[test]
fn a_text_input_has_a_darker_bottom_edge() {
    if !ui_available() {
        return;
    }
    let (f, _) = render(FORM, "input");
    f.expect(120, 80, "#ffffff", "the field's ground");
    f.expect(20, 86, "#d1d1d1", "the field's left border");
    f.expect(120, 70, "#d1d1d1", "the field's top border");
    // The field is 32 tall from y=70, so its last row is y=101.
    f.expect(120, 101, "#8a8a8a", "the field's bottom border");
}

/// A ticked checkbox fills with the accent. There is no tick glyph — an
/// `<input>` holds no content — so the box is solid and its middle is the
/// accent exactly.
#[test]
fn a_ticked_checkbox_fills_with_the_accent() {
    if !ui_available() {
        return;
    }
    let (f, _) = render(FORM, "checked");
    f.expect(27, 127, "#005fb8", "the ticked box's fill");
}

/// RCSS is not CSS: a property it does not know is a "Syntax error parsing
/// property declaration" on stderr and a rule that silently does nothing.
/// The stylesheet must parse clean, or the colours above are the only part of
/// it anyone ever checked.
#[test]
fn the_stylesheet_parses_without_a_syntax_error() {
    if !ui_available() {
        return;
    }
    let (_, stderr) = render(FORM, "parse");
    assert!(
        !stderr.contains("Syntax error"),
        "the substrate refused part of the stylesheet:\n{stderr}"
    );
}
