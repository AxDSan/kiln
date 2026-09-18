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
        format!(
            "#{:02x}{:02x}{:02x}",
            self.px[i],
            self.px[i + 1],
            self.px[i + 2]
        )
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
    render_with(src, tag, &[])
}

/// The same, with environment variables the run needs — a theme override, or a
/// synthetic click.
fn render_with(src: &str, tag: &str, env: &[(&str, &str)]) -> (Frame, String) {
    render_project(src, tag, env, &[])
}

/// The same, with files written beside the source first — a project's
/// `themes/ocean.ktheme`, say, which the build embeds.
fn render_project(src: &str, tag: &str, env: &[(&str, &str)], files: &[(&str, &str)])
    -> (Frame, String) {
    let dir = std::env::temp_dir().join(format!("kiln_style_{tag}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("main.kiln");
    std::fs::write(&source, src).expect("write source");
    for (path, body) in files {
        let at = dir.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).expect("theme directory");
        std::fs::write(at, body).expect("write file beside the source");
    }
    let bin = dir.join("prog");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args([
            "build",
            source.to_str().unwrap(),
            "-o",
            bin.to_str().unwrap(),
        ])
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln");
    assert!(status.success(), "kiln build failed for {tag}");

    let dump = dir.join("frame.ppm");
    let mut run = Command::new(&bin);
    run.env("KILN_UI_EXIT_AFTER_FRAMES", "4").env("KILN_UI_DUMP", &dump);
    for (k, v) in env {
        run.env(k, v);
    }
    let out = run
        .output()
        .expect("run built binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "binary exited non-zero\nstderr:\n{stderr}"
    );
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
    assert_eq!(
        (f.width, f.height),
        (400, 300),
        "the dump is not the form's size"
    );

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
/// A form that names a theme is drawn from that theme's palette, and a program
/// that switches theme while it runs redraws every control from the new one
/// without losing a value or a handler.
///
/// In pixels, because a theme is a stylesheet: a token that never reaches the
/// screen reads fine in the source.
#[test]
fn a_theme_paints_the_whole_form() {
    if !ui_available() {
        return;
    }
    const THEMED: &str = r#"namespace Themed;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "themed";
    Width = 400;
    Height = 300;
    Theme = "Dark";

    Button ok { Text = "OK"; Left = 20; Top = 20; Width = 96; Height = 32; }
    Editbox name { Text = "Ada"; Left = 20; Top = 70; Width = 200; Height = 32; }
}
"#;
    let (f, _) = render(THEMED, "theme_dark");
    f.expect(2, 2, "#1f1f1f", "the dark theme's ground");
    f.expect(30, 36, "#333333", "the button's plate in the dark theme");
    f.expect(20, 36, "#4a4a4a", "the button's outline in the dark theme");

    // The same form, the same source, one environment variable: what a
    // developer looking at a program in another palette gets, and it outranks
    // the form's own choice.
    let (light, _) = render_with(THEMED, "theme_env", &[("KILN_UI_THEME", "HighContrast")]);
    light.expect(2, 2, "#ffffff", "the high-contrast ground");
    light.expect(20, 36, "#000000", "the high-contrast outline");
}

/// The classic theme is a bevel rather than an outline: the face is the grey
/// that system drew every control in, the top and left edges are white, and the
/// bottom and right edges are black. A corner radius would give it away, so the
/// plate is checked a pixel in from its own edge.
#[test]
fn the_classic_theme_is_a_windows_bevel() {
    if !ui_available() {
        return;
    }
    const CLASSIC: &str = r#"namespace Classic;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "classic";
    Width = 400;
    Height = 300;
    Theme = "Classic";

    Button ok { Text = "OK"; Left = 20; Top = 20; Width = 96; Height = 32; }
    Editbox name { Text = "Ada"; Left = 20; Top = 70; Width = 200; Height = 32; }
}
"#;
    let (f, _) = render(CLASSIC, "theme_classic");
    f.expect(2, 2, "#c0c0c0", "the classic ground");
    f.expect(26, 46, "#c0c0c0", "the button's face");
    f.expect(20, 36, "#ffffff", "the button's lit left edge");
    f.expect(115, 36, "#000000", "the button's shaded right edge");
    f.expect(60, 20, "#ffffff", "the button's lit top edge");
    // A field is the same bevel the other way round, over white paper.
    f.expect(20, 86, "#808080", "the field's sunk left edge");
    f.expect(150, 86, "#ffffff", "the field's paper");
}

/// A theme the project ships: `themes/<name>.ktheme` beside the source, carried
/// into the binary and applied by name. It sets the tokens it cares about over
/// the theme it names as its base, and leaves the rest of that theme alone.
#[test]
fn a_project_ships_its_own_theme() {
    if !ui_available() {
        return;
    }
    const OCEAN: &str = r##"{
  "base": "Dark",
  "name": "Ocean",
  "ground": "#0b2942",
  "control": "#164a73",
  "accent": "#3fc1ff",
  "text": "#eaf6ff"
}
"##;
    const APP: &str = r#"namespace Sea;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "ocean";
    Width = 400;
    Height = 300;
    Theme = "Ocean";

    Button ok { Text = "OK"; Left = 20; Top = 20; Width = 96; Height = 32; }
}
"#;
    let (f, _) = render_project(APP, "theme_file", &[], &[("themes/ocean.ktheme", OCEAN)]);
    f.expect(2, 2, "#0b2942", "the project theme's ground");
    f.expect(26, 46, "#164a73", "the project theme's control");
    // A token the file did not set is the base theme's, not the default's.
    f.expect(20, 46, "#4a4a4a", "the dark base's control outline");

    // A name no theme file answers to is refused, and the form keeps the theme
    // it had — a palette that silently half-applied is the failure nobody sees.
    const MISSING: &str = r#"namespace Missing;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "missing";
    Width = 400;
    Height = 300;
    Theme = "NoSuchTheme";

    Button ok { Text = "OK"; Left = 20; Top = 20; Width = 96; Height = 32; }
}
"#;
    let (g, _) = render(MISSING, "theme_missing");
    g.expect(2, 2, "#f3f3f3", "an unknown theme leaves the default in place");
}

/// Switching while the program runs: the button's handler asks for `Light`, and
/// the frame after the click is the light palette — including the label, which
/// reads back the theme now in force.
#[test]
fn a_running_program_switches_theme() {
    if !ui_available() {
        return;
    }
    const SWITCH: &str = r#"namespace Switcher;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "switch";
    Width = 400;
    Height = 300;
    Theme = "Dark";

    Label caption { Text = "?"; Left = 20; Top = 120; Width = 200; Height = 24; }
    Button swap { Text = "Light"; Left = 20; Top = 20; Width = 96; Height = 32; Click += OnSwap; }
}

public partial form MainWindow
{
    void OnSwap()
    {
        Ui.SetTheme("Light");
        caption.Text = Ui.Theme();
    }
}
"#;
    // Handle 3 is the button: the form is 1, the label 2.
    let (before, _) = render(SWITCH, "switch_before");
    before.expect(2, 2, "#1f1f1f", "the form starts dark");
    let (after, _) = render_with(SWITCH, "switch_after", &[("KILN_UI_SYNTH_CLICK", "3")]);
    after.expect(2, 2, "#f3f3f3", "the click repainted the form light");
    after.expect(30, 36, "#ffffff", "and the button with it");
}

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
