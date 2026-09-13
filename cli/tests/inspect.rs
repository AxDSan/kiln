//! The lines the designer reads: `kiln inspect` and `kiln commands`.
//!
//! Studio never parses `.kiln` itself, so these two listings are its entire
//! knowledge of a file and of a library. A line kind that changes shape, or a
//! value that spills onto a second line, is not a formatting bug there — it is
//! a save that writes something else back.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_inspect_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

fn kiln(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(cwd)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "kiln {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has_line(out: &str, line: &str) -> bool {
    out.lines().any(|l| l == line)
}

const SOURCE: &str = "module rt
use ui

form win
  title = \"Round trip\"

  memo notes
    text = \"first\\nsecond \\\\ back\"
  end
end

sub on_tick
  if 1 > 2
    call print_text(\"never\")
  end
end

record point
  x: int
end

timer ticker
  interval = 500
  on tick: on_tick
end
";

#[test]
fn module_components_are_a_distinct_line_kind_with_a_span() {
    let dir = scratch("modcomponent");
    std::fs::write(dir.join("rt.kiln"), SOURCE).unwrap();
    let out = kiln(&dir, &["inspect", "rt.kiln"]);

    // `record point` is the same two tokens as a component header, and the
    // sub above it holds a nested `end`; neither may shift the span.
    assert!(
        has_line(&out, "modcomponent: ticker timer span=22..25"),
        "{out}"
    );
    assert!(has_line(&out, "prop: ticker interval 500"), "{out}");
    assert!(has_line(&out, "handler: ticker tick on_tick"), "{out}");
    // Never as a form child: the designer would write it back inside the form.
    assert!(!has_line(&out, "component: ticker timer"), "{out}");
    assert!(has_line(&out, "component: notes memo"), "{out}");
    assert!(has_line(&out, "form: win span=4..10"), "{out}");
}

#[test]
fn a_property_value_stays_on_one_line() {
    let dir = scratch("escape");
    std::fs::write(dir.join("rt.kiln"), SOURCE).unwrap();
    let out = kiln(&dir, &["inspect", "rt.kiln"]);

    assert!(
        has_line(&out, "prop: notes text first\\nsecond \\\\ back"),
        "{out}"
    );
    // Every line announces its kind — nothing is left for a reader to guess
    // is a continuation of the value above it.
    for l in out.lines() {
        let kind = l.split(": ").next().unwrap_or("");
        assert!(
            matches!(
                kind,
                "module"
                    | "use"
                    | "sub"
                    | "subsig"
                    | "form"
                    | "component"
                    | "modcomponent"
                    | "prop"
                    | "handler"
            ),
            "unlabelled line {l:?} in:\n{out}"
        );
    }
}

#[test]
fn commands_reports_kind_and_editor() {
    let dir = scratch("commands");
    let core = kiln(&dir, &["commands"]);
    assert!(has_line(&core, "component: timer"), "{core}");
    assert!(has_line(&core, "kind: timer nonvisual"), "{core}");

    let ui = kiln(&dir, &["commands", "--use", "ui"]);
    assert!(has_line(&ui, "kind: button visual"), "{ui}");
    assert!(has_line(&ui, "editor: memo text multiline"), "{ui}");
    // The lines a reader already parses are untouched: an `editor:` line is
    // added beside a `property:` line, never folded into it.
    assert!(has_line(&ui, "property: memo text text"), "{ui}");
    assert!(has_line(&ui, "event: button click"), "{ui}");
    for l in ui.lines() {
        let kind = l.split(": ").next().unwrap_or("");
        assert!(
            matches!(
                kind,
                "command"
                    | "component"
                    | "kind"
                    | "property"
                    | "editor"
                    | "event"
                    // A documented command carries its sentence and its
                    // sample beside its signature. Core's first documented
                    // commands are the byte-set bridge, so this listing
                    // grew these two kinds — added lines, never folded into
                    // the `command:` line a reader already parses.
                    | "doc"
                    | "example"
            ),
            "unexpected line {l:?}"
        );
    }
}

#[test]
fn a_kiln_2_form_inspects_in_the_same_lines() {
    // Studio's entire knowledge of a file is these lines, and it must not grow
    // a second parser to read a K2 one. So a K2 form reports in exactly the
    // shapes a 1.x form does — `form:`, `component:`, `prop:`, `handler:`,
    // `sub:` — and a reader written for 1.x reads it unchanged.
    let dir = scratch("k2form");
    let src = "\
namespace Counter;

public partial form MainWindow
{
    Title = \"Counter\";
    Width = 320;

    Label count { Text = \"0\"; Left = 40; }
    Button add { Text = \"Add one\"; Click += OnAdd; }
}

public partial form MainWindow
{
    int n;
    void OnAdd() { n = n + 1; count.Text = $\"{n}\"; }
}
";
    let path = dir.join("counter.kiln");
    std::fs::write(&path, src).unwrap();
    let out = kiln(&dir, &["inspect", "counter.kiln"]);

    assert!(out.contains("module: Counter"), "{out}");
    // Names are reported in the spelling the toolchain files things under:
    // the registry's component is `label`, not `Label`, and the designer looks
    // a descriptor up by that name. Same rule the spec gives for commands.
    assert!(out.contains("sub: on_add"), "{out}");
    // One form, though it is written in two halves.
    assert_eq!(
        out.lines().filter(|l| l.starts_with("form: ")).count(),
        1,
        "a partial form must report once:\n{out}"
    );
    assert!(out.contains("form: main_window span="), "{out}");
    assert!(out.contains("prop: main_window title Counter"), "{out}");
    assert!(out.contains("prop: main_window width 320"), "{out}");
    assert!(out.contains("component: count label"), "{out}");
    assert!(out.contains("component: add button"), "{out}");
    assert!(out.contains("prop: add text Add one"), "{out}");
    assert!(out.contains("handler: add click on_add"), "{out}");
}

#[test]
fn a_kiln_2_using_reads_as_a_library_name() {
    // 1.x says `use file`; K2 says `using Kiln.File;`. The designer wants the
    // library, so both report the same `use:` line.
    let dir = scratch("k2use");
    let src = "namespace U;\nusing Kiln.File;\npublic static class P\n{\n    public static void Main() { }\n}\n";
    std::fs::write(dir.join("u.kiln"), src).unwrap();
    let out = kiln(&dir, &["inspect", "u.kiln"]);
    assert!(out.contains("use: file"), "{out}");
}
