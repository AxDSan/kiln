//! The first hour: `kiln templates`, `kiln new`, and whether what comes
//! out of them actually builds.
//!
//! A template is the first Kiln code anyone reads, and a broken one is not
//! a broken example — it is the language failing on the first thing the user
//! tried. So every template is instantiated and built here through the real
//! binary, not inspected as text: a template that parses and does not link is
//! exactly the failure a text check misses.
//!
//! `HOME` and the working directory are pinned in every test, because both are
//! inputs to kit resolution and therefore to the template listing. A test that
//! inherited the developer's would pass or fail on what they happen to have
//! installed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A scratch directory unique to `tag`. Tests run in parallel and two of them
/// sharing a path race.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_dx_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

fn kiln(cwd: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln")
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = kiln(cwd, home, args);
    assert!(
        out.status.success(),
        "kiln {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `id -> target`, from the `template:` lines of `kiln templates`.
///
/// Read from the same directory the projects are created in: a kit in a
/// `kits/` directory above the caller contributes templates, so a listing
/// taken somewhere else is a listing of a different set.
fn listed_templates(cwd: &Path, home: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in ok(cwd, home, &["templates"]).lines() {
        if let Some(rest) = line.strip_prefix("template: ") {
            if let Some((id, target)) = rest.split_once(' ') {
                out.insert(id.to_string(), target.to_string());
            }
        }
    }
    out
}

/// The GUI target links the vendored UI stack, which not every machine has
/// fetched. Skipping is honest; pretending to have tested it is not.
fn ui_vendored() -> bool {
    repo().join("vendor/RmlUi/build/librmlui.a").exists()
}

/// Every bundled template must build. This is the whole reason the file
/// exists: a template is the one piece of Kiln code a new user did not
/// write and cannot debug.
#[test]
fn every_bundled_template_creates_and_builds() {
    let home = scratch("tmpl_home");
    let root = scratch("tmpl_root");
    let templates = listed_templates(&root, &home);
    assert!(
        templates.len() >= 4,
        "the bundled set went missing: {templates:?}"
    );

    for (id, target) in &templates {
        if target == "gui" && !ui_vendored() {
            eprintln!("RmlUi not vendored (run tools/fetch-rmlui.sh); skipping `{id}`");
            continue;
        }
        let dest = root.join(id);
        let created = ok(&root, &home, &["new", id, dest.to_str().unwrap()]);
        assert!(created.contains(&format!("created: {id}")), "{created}");

        // The `open:` line is what Studio opens after creating a project, so a
        // path that is not there is a New Project dialog that opens nothing.
        let entry = created
            .lines()
            .find_map(|l| l.strip_prefix("open: "))
            .unwrap_or_else(|| panic!("no `open:` line for {id}: {created}"));
        assert!(
            Path::new(entry).is_file(),
            "`{id}` points at {entry}, which was not written"
        );

        let out = kiln(
            &root,
            &home,
            &["build", entry, "-o", dest.join("built").to_str().unwrap()],
        );
        assert!(
            out.status.success(),
            "template `{id}` does not build:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `__MODULE__` must be substituted everywhere, or the first build fails on a
/// name the user never typed.
#[test]
fn a_new_project_carries_no_placeholders() {
    let home = scratch("subst_home");
    let root = scratch("subst_root");
    for id in listed_templates(&root, &home).keys() {
        let dest = root.join(id);
        ok(&root, &home, &["new", id, dest.to_str().unwrap()]);
        for entry in std::fs::read_dir(&dest).expect("read project").flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !text.contains("__MODULE__"),
                "`{id}` left a placeholder in {}",
                entry.path().display()
            );
        }
        assert!(
            !dest.join("template.meta").exists(),
            "`{id}` copied its own metadata into the project"
        );
    }
}

/// The timer template's whole claim is that the program outlives `main` and
/// then stops by itself. Both halves matter: one that never quits is a hang
/// the user has to learn to Ctrl-C out of.
#[test]
fn a_timer_program_outlives_main_and_quits_itself() {
    let home = scratch("timer_home");
    let root = scratch("timer_root");
    let example = repo().join("examples/loopdemo.kiln");

    let mut child = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["run", example.to_str().unwrap()])
        .current_dir(&root)
        .env("HOME", &home)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("run the timer project");

    // Polled rather than waited on: a program that never calls `quit` would
    // hang the whole test run, which is the exact failure being tested for.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().expect("poll") {
            Some(status) => {
                assert!(status.success(), "the timer example exited with {status}");
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("the timer project never quit — `quit()` did not end the loop");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    let mut text = String::new();
    use std::io::Read;
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut text)
        .expect("read output");
    assert!(text.contains("main returned"), "main never ran: {text}");
    assert!(text.contains("tick 3"), "the tick handler never ran to the end: {text}");
}

/// `kiln new` on a name that does not exist must say what does. A bare
/// "no such template" leaves the user with nothing to type next.
#[test]
fn an_unknown_template_names_the_ones_that_exist() {
    let home = scratch("unknown_home");
    let root = scratch("unknown_root");
    let out = kiln(
        &root,
        &home,
        &["new", "consoleapp", root.join("x").to_str().unwrap()],
    );
    assert!(!out.status.success(), "a missing template is an error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("console-app"), "must list the real ids: {err}");
}

/// The compiler's own output names the fix too: a typo says which command was
/// meant, at the line it is on. The `line N:` shape is what Studio parses.
#[test]
fn a_typo_in_a_command_names_the_nearest_one() {
    let home = scratch("typo_home");
    let root = scratch("typo_root");
    let src = root.join("main.kiln");
    std::fs::write(&src, "module m\nsub main\n  call prnt_text(\"hi\")\nend\n").unwrap();
    let out = kiln(&root, &home, &["emit", src.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("line 3: in `main`: unknown command `prnt_text` — did you mean `print_text`?"),
        "{err}"
    );
}

/// A windowed program that reaches for `sys_sleep_ms` is refused at build
/// time and pointed at `timer` — the event loop, taught at the moment it has
/// to be learned. `emit` runs the same validation as `build` without needing
/// the UI stack to be linkable.
#[test]
fn a_windowed_program_is_refused_sleep_and_pointed_at_timer() {
    let home = scratch("sleep_home");
    let root = scratch("sleep_root");
    let src = root.join("main.kiln");
    std::fs::write(
        &src,
        //  1         2       3           4         5      6         7                        8
        "module m\nuse ui\nuse system\nform win\nend\nsub main\n  call sys_sleep_ms(500)\nend\n",
    )
    .unwrap();
    let out = kiln(&root, &home, &["emit", src.to_str().unwrap()]);
    assert!(!out.status.success(), "a sleep in a windowed program must not build");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(
            "line 7: in `main`: `sys_sleep_ms` would freeze the window — a windowed program \
             does not wait, it declares a `timer` and does the work in its `on tick` handler"
        ),
        "{err}"
    );

    // The same call in a console program is fine: there is no window to freeze.
    std::fs::write(
        &src,
        "module m\nuse system\nsub main\n  call sys_sleep_ms(1)\nend\n",
    )
    .unwrap();
    let out = kiln(&root, &home, &["emit", src.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
