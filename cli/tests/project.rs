//! The project file: `kiln new` writes one, `kiln project` reads it
//! back, and build/run/inspect take it in place of the `.kiln`.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Run from `cwd`, which the tests set deliberately AWAY from the project: a
/// `main:` resolved against the working directory instead of the project file
/// works from inside the project and nowhere else.
fn kiln(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(cwd)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l == line)
}

/// A fresh console project in a scratch directory, created from a working
/// directory that is not the project's.
fn fresh(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_proj_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = kiln(&repo(), &["new", "console-app", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    dir
}

#[test]
fn new_writes_a_project_file_and_reports_it() {
    let dir = fresh("new");
    let proj = dir.join("project.kproj");
    assert!(proj.is_file(), "no project file written");
    let text = std::fs::read_to_string(&proj).unwrap();
    assert!(has_line(&text, "name: kiln_proj_new"), "{text}");
    assert!(has_line(&text, "main: main.kiln"), "{text}");
    assert!(has_line(&text, "target: console"), "{text}");
    assert!(has_line(&text, "version: 0.1.0"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `kits:` is derived from the entry's own `use` lines, so a fresh checkout
/// of a project knows what to install without anyone maintaining a list.
#[test]
fn kits_follow_the_entry_file() {
    let dir = std::env::temp_dir().join("kiln_proj_kits");
    let _ = std::fs::remove_dir_all(&dir);
    let out = kiln(&repo(), &["new", "gui-app", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = std::fs::read_to_string(dir.join("project.kproj")).unwrap();
    let src = std::fs::read_to_string(dir.join("main.kiln")).unwrap();
    let uses: Vec<&str> = src
        .lines()
        .filter_map(|l| l.strip_prefix("use "))
        .map(str::trim)
        .collect();
    assert!(!uses.is_empty(), "the template should `use` something");
    assert!(
        has_line(&text, &format!("kits: {}", uses.join(" "))),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The file and its directory name the same project, and `main:` comes back
/// resolved so a reader standing anywhere can open it.
#[test]
fn project_prints_resolved_fields_for_file_and_directory() {
    let dir = fresh("print");
    let by_dir = kiln(&repo(), &["project", dir.to_str().unwrap()]);
    let by_file = kiln(
        &repo(),
        &["project", dir.join("project.kproj").to_str().unwrap()],
    );
    assert!(by_dir.status.success(), "{}", stderr(&by_dir));
    assert_eq!(stdout(&by_dir), stdout(&by_file), "file and directory disagree");
    let text = stdout(&by_dir);
    assert!(has_line(&text, "name: kiln_proj_print"), "{text}");
    assert!(
        has_line(&text, &format!("main: {}", dir.join("main.kiln").display())),
        "{text}"
    );
    assert!(has_line(&text, "target: console"), "{text}");
    assert!(has_line(&text, "version: 0.1.0"), "{text}");
    // Every line announces its kind, like `inspect`.
    for l in text.lines() {
        let kind = l.split(": ").next().unwrap_or("");
        assert!(
            matches!(kind, "project" | "name" | "main" | "target" | "kit" | "version"),
            "unlabelled line {l:?} in:\n{text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_and_run_take_the_project_from_elsewhere() {
    let dir = fresh("build");
    // From the repository root, not the project: the entry is relative to the
    // project file, and the output lands in the project, not here.
    let built = kiln(&repo(), &["build", dir.to_str().unwrap()]);
    assert!(built.status.success(), "{}", stderr(&built));
    assert!(dir.join("kiln_proj_build").is_file(), "output not in the project");
    assert!(!repo().join("kiln_proj_build").exists(), "output leaked into cwd");

    let ran = kiln(
        &repo(),
        &["run", dir.join("project.kproj").to_str().unwrap()],
    );
    assert!(ran.status.success(), "{}", stderr(&ran));
    assert!(stdout(&ran).contains("Hello from Kiln"), "{}", stdout(&ran));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `inspect` is how Studio reads a file; handed a project it must describe the
/// project's entry, so a project path reaching Studio opens the right module.
#[test]
fn inspect_describes_the_projects_entry() {
    let dir = fresh("inspect");
    let out = kiln(&repo(), &["inspect", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(has_line(&stdout(&out), "module: kiln_proj_inspect"), "{}", stdout(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The project's `target:` applies when the command line gives none, and a
/// `--target` on the command line still wins.
#[test]
fn the_projects_target_yields_to_the_flag() {
    let dir = fresh("target");
    let proj = dir.join("project.kproj");
    let text = std::fs::read_to_string(&proj)
        .unwrap()
        .replace("target: console", "target: sharedlib");
    std::fs::write(&proj, text).unwrap();

    let built = kiln(&repo(), &["build", dir.to_str().unwrap()]);
    assert!(built.status.success(), "{}", stderr(&built));
    assert!(dir.join("libkiln_proj_target.so").is_file(), "project target ignored");

    let built = kiln(&repo(), &["build", dir.to_str().unwrap(), "--target", "console"]);
    assert!(built.status.success(), "{}", stderr(&built));
    assert!(dir.join("kiln_proj_target").is_file(), "--target did not win");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_directory_without_a_project_is_refused_by_name() {
    let dir = std::env::temp_dir().join("kiln_proj_none");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = kiln(&repo(), &["build", dir.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("project.kproj"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_names_the_toolchain_and_the_abi() {
    for flag in ["version", "--version", "-V"] {
        let out = kiln(&repo(), &[flag]);
        assert!(out.status.success());
        let text = stdout(&out);
        assert!(
            has_line(&text, &format!("kiln {}", env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
        let abi = std::fs::read_to_string(repo().join("abi/kiln_abi.h"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("#define KILN_ABI_VERSION"))
            .map(|v| v.trim().to_string())
            .expect("the header defines the ABI version");
        assert!(has_line(&text, &format!("abi {abi}")), "{text}");
    }
}

/// The terminal says which library has a command, as the editor already did.
#[test]
fn build_says_which_library_has_an_unknown_command() {
    let dir = std::env::temp_dir().join("kiln_proj_hint");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("hint.kiln"),
        "module hint\nsub main\n  call print_text(file_read_text(\"x\"))\nend\n",
    )
    .unwrap();
    let out = kiln(&dir, &["build", "hint.kiln"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("it is in the `file` library: add `use file` to the module"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The caption a GUI project opens with is a title, not a module identifier:
/// "Untitled App" until `--title` names it, whatever the folder is called.
#[test]
fn a_gui_project_is_untitled_until_titled() {
    let repo = repo();
    let dir = std::env::temp_dir().join("kiln_proj_untitled");
    let _ = std::fs::remove_dir_all(&dir);
    let out = kiln(&repo, &["new", "gui-app", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(has_line(&stdout(&out), "title: Untitled App"), "{}", stdout(&out));
    let src = std::fs::read_to_string(dir.join("main.kiln")).unwrap();
    assert!(src.contains("title = \"Untitled App\""), "{src}");
    assert!(src.contains("module kiln_proj_untitled"), "the module still follows the folder:\n{src}");
    let _ = std::fs::remove_dir_all(&dir);

    let out = kiln(&repo, &["new", "gui-app", dir.to_str().unwrap(), "--title", "Inventory"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let src = std::fs::read_to_string(dir.join("main.kiln")).unwrap();
    assert!(src.contains("title = \"Inventory\""), "{src}");
    let _ = std::fs::remove_dir_all(&dir);
}
