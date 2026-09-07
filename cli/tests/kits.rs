//! Kit resolution, `kiln kit add`, and kit-shipped templates.
//!
//! Every test runs with its own `HOME` and its own working directory, because
//! both are inputs to resolution: a test that inherited the developer's `HOME`
//! would pass or fail depending on what they happen to have installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A scratch directory unique to `tag`. Tests run in parallel and two of them
/// sharing a path race — the same hazard `build.rs` documents for binaries.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_kits_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// Write a compilable kit that exports `<name>_answer() -> int`.
fn write_kit(dir: &Path, name: &str, version: &str) {
    std::fs::create_dir_all(dir).expect("create kit dir");
    std::fs::write(
        dir.join(format!("{name}_libinfo.c")),
        format!(
            r#"#include "kiln_abi.h"
void {name}_answer(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv);
static const Kiln_CommandDesc C[] = {{
    {{ "{name}_answer", "{name}_answer", KN_SDT_INT, 0, 0 }},
}};
static const Kiln_LibInfo I = {{
    KILN_ABI_VERSION, "{name}", "kiln-test-{name}", 1, 0, 0,
    (int32_t)(sizeof(C) / sizeof(C[0])), C,
}};
const Kiln_LibInfo *kiln_get_lib_info(void) {{ return &I; }}
"#
        ),
    )
    .expect("write libinfo");
    std::fs::write(
        dir.join(format!("{name}_cmds.c")),
        format!(
            r#"#include "kiln_abi.h"
void {name}_answer(Kiln_Slot *ret, int32_t argc, Kiln_Slot *argv) {{
    (void)argc; (void)argv; kn_ret_int(ret, 42);
}}
"#
        ),
    )
    .expect("write cmds");
    std::fs::write(
        dir.join("lib.json"),
        format!("{{ \"display\": \"{name} kit\", \"version\": \"{version}\" }}\n"),
    )
    .expect("write manifest");
}

/// Run `kiln` with `cwd` and `home` both pinned.
fn kiln(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");
    assert!(
        out.status.success(),
        "kiln {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The `kit:` line for `name`, or None when resolution found nothing.
fn kit_line(listing: &str, name: &str) -> Option<String> {
    listing
        .lines()
        .find(|l| l.starts_with(&format!("kit: {name} ")))
        .map(str::to_string)
}

fn path_line(listing: &str, name: &str) -> Option<String> {
    listing
        .lines()
        .find(|l| l.starts_with(&format!("path: {name} ")))
        .map(|l| l["path: ".len() + name.len() + 1..].to_string())
}

/// A bundled library needs no manifest to be a kit — every existing one has
/// none, and they must keep listing and keep working.
#[test]
fn bundled_libraries_resolve_without_a_manifest() {
    let home = scratch("bundled_home");
    let listing = kiln(&repo(), &home, &["kits"]);
    assert_eq!(
        kit_line(&listing, "file").as_deref(),
        Some("kit: file 0.0.0 bundled")
    );
    assert_eq!(
        path_line(&listing, "file"),
        Some(repo().join("libs/file").display().to_string())
    );
}

/// The kit shipped at `<repo>/kits/` is found because the repository is the
/// project when you are standing in it.
#[test]
fn the_shipped_kit_resolves_as_a_project_kit() {
    let home = scratch("units_home");
    let listing = kiln(&repo(), &home, &["kits"]);
    assert_eq!(
        kit_line(&listing, "units").as_deref(),
        Some("kit: units 1.0.0 project")
    );
    assert_eq!(
        path_line(&listing, "units"),
        Some(repo().join("kits/units").display().to_string())
    );
    assert!(listing.contains("section: units Measurement"));
    assert!(listing.contains("name: units Units"));
}

/// Project beats user beats bundled, and the listing says which won — the whole
/// point of printing the tier and the path.
#[test]
fn resolution_order_is_project_then_user_then_bundled() {
    let root = scratch("order");
    let home = root.join("home");
    let user_kit = home.join(".kiln/kits/file");
    write_kit(&user_kit, "file", "9.9.9");

    // From a directory with no `kits/` above it, the user tier shadows the
    // bundled `libs/file`.
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let listing = kiln(&elsewhere, &home, &["kits"]);
    assert_eq!(
        kit_line(&listing, "file").as_deref(),
        Some("kit: file 9.9.9 user")
    );

    // Put one in the project and it shadows the user's in turn.
    let project = root.join("project");
    write_kit(&project.join("kits/file"), "file", "0.1.0");
    let listing = kiln(&project, &home, &["kits"]);
    assert_eq!(
        kit_line(&listing, "file").as_deref(),
        Some("kit: file 0.1.0 project")
    );
    assert_eq!(
        path_line(&listing, "file"),
        Some(project.join("kits/file").display().to_string())
    );

    // With neither present the bundled library is what is left.
    let bare = scratch("order_bare");
    let listing = kiln(&elsewhere, &bare, &["kits"]);
    assert_eq!(
        kit_line(&listing, "file").as_deref(),
        Some("kit: file 0.0.0 bundled")
    );
}

/// `use` must reach the kit resolution chose, not only `libs/`: a kit you can
/// list but not call is a listing that lies.
#[test]
fn a_user_kit_shadows_the_bundled_library_for_use_too() {
    let root = scratch("shadow_use");
    let home = root.join("home");
    write_kit(&home.join(".kiln/kits/hello"), "hello", "2.0.0");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let out = kiln(&elsewhere, &home, &["commands", "--use", "hello"]);
    assert!(
        out.contains("command: hello_answer() -> int"),
        "the shadowing kit's command is missing:\n{out}"
    );
    assert!(
        !out.contains("command: greet("),
        "the bundled libs/hello was loaded instead of the kit:\n{out}"
    );
}

/// `kiln kit add` from a directory, then `kiln commands --use`.
#[test]
fn kit_add_from_a_directory_then_use_it() {
    let root = scratch("add_dir");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let src = root.join("src/addkit");
    write_kit(&src, "addkit", "3.1.4");

    let out = kiln(&root, &home, &["kit", "add", src.to_str().unwrap()]);
    assert!(out.contains("installed: addkit 3.1.4"), "{out}");
    assert!(out.contains("action: addkit added"), "{out}");
    assert!(
        home.join(".kiln/kits/addkit/addkit_libinfo.c").is_file(),
        "the kit was not unpacked into ~/.kiln/kits"
    );

    let listing = kiln(&root, &home, &["kits"]);
    assert_eq!(
        kit_line(&listing, "addkit").as_deref(),
        Some("kit: addkit 3.1.4 user")
    );
    let cmds = kiln(&root, &home, &["commands", "--use", "addkit"]);
    assert!(cmds.contains("command: addkit_answer() -> int"), "{cmds}");

    // Installing again over the top says so rather than silently succeeding.
    let out = kiln(&root, &home, &["kit", "add", src.to_str().unwrap()]);
    assert!(out.contains("action: addkit replaced"), "{out}");
}

/// The same, from a tarball — the shape a kit is actually distributed in.
#[test]
fn kit_add_from_a_tarball_then_build_a_program_with_it() {
    let root = scratch("add_tar");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let stage = root.join("stage");
    write_kit(&stage.join("tarkit"), "tarkit", "0.2.0");
    let tarball = root.join("tarkit.tar.gz");
    let status = Command::new("tar")
        .args([
            "-czf",
            tarball.to_str().unwrap(),
            "-C",
            stage.to_str().unwrap(),
            "tarkit",
        ])
        .status()
        .expect("run tar");
    assert!(status.success(), "tar failed");

    let out = kiln(&root, &home, &["kit", "add", tarball.to_str().unwrap()]);
    assert!(out.contains("installed: tarkit 0.2.0"), "{out}");

    // All the way through: a program that `use`s an installed kit compiles,
    // links and runs.
    let src = root.join("main.kiln");
    std::fs::write(
        &src,
        "module tarapp\nuse tarkit\n\nsub main\n  call print_int(tarkit_answer())\nend\n",
    )
    .unwrap();
    let bin = root.join("tarapp");
    kiln(
        &root,
        &home,
        &["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()],
    );
    let run = Command::new(&bin).output().expect("run built binary");
    assert!(run.status.success(), "built program exited non-zero");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
}

/// A kit ships templates, and `kiln new` can instantiate one — a tile you
/// can see but not click is worse than no tile.
///
/// The kit is staged here rather than shipped: a template only exists to be a
/// starting point, and the bundled set is deliberately the four shapes a person
/// actually starts from. Everything else is an example.
#[test]
fn kit_templates_are_listed_and_can_be_created() {
    let home = scratch("tmpl_home");
    let root = scratch("tmpl_root");
    let kit = root.join("kits/gadget");
    write_kit(&kit, "gadget", "1.0.0");
    std::fs::write(
        kit.join("lib.json"),
        r#"{ "display": "Gadget", "templates": ["gadget-app"] }"#,
    )
    .unwrap();
    let tmpl = kit.join("gadget-app");
    std::fs::create_dir_all(&tmpl).unwrap();
    std::fs::write(
        tmpl.join("template.meta"),
        "name: Gadget App\ndesc: Uses the gadget kit.\ntarget: console\n",
    )
    .unwrap();
    std::fs::write(
        tmpl.join("main.kiln"),
        "module __MODULE__\nuse gadget\n\nsub main\n  call print_int(gadget_answer())\nend\n",
    )
    .unwrap();

    let listing = kiln(&root, &home, &["templates"]);
    assert!(listing.contains("template: gadget-app console"), "{listing}");
    assert!(listing.contains("name: gadget-app Gadget App"), "{listing}");

    // The bundled templates are untouched: the designer parses this listing.
    for id in ["console-app", "gui-app", "shared-library", "static-library"] {
        assert!(
            listing.contains(&format!("template: {id} ")),
            "bundled template `{id}` disappeared from the listing"
        );
    }

    let dest = root.join("widget");
    let out = kiln(&root, &home, &["new", "gadget-app", dest.to_str().unwrap()]);
    assert!(out.contains("created: gadget-app"), "{out}");
    let main = std::fs::read_to_string(dest.join("main.kiln")).expect("template file copied");
    assert!(main.contains("module widget"), "__MODULE__ not replaced");
    assert!(main.contains("use gadget"));
}

/// The shipped kit compiles and runs, so the proof kit is a kit and not a
/// directory that merely looks like one.
#[test]
fn the_shipped_kit_builds_and_runs() {
    let root = scratch("units_build");
    let home = scratch("units_build_home");
    let src = root.join("main.kiln");
    std::fs::write(
        &src,
        "module unitsapp\nuse units\n\nsub main\n  call print_double(units_c_to_f(100.0))\n  call print_double(units_f_to_c(32.0))\nend\n",
    )
    .unwrap();
    let bin = root.join("unitsapp");
    // Run from the repository, which is where `kits/units` is the project kit.
    kiln(
        &repo(),
        &home,
        &["build", src.to_str().unwrap(), "-o", bin.to_str().unwrap()],
    );
    let run = Command::new(&bin).output().expect("run built binary");
    assert!(run.status.success());
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert_eq!(stdout.lines().collect::<Vec<_>>(), vec!["212", "0"]);
}

/// A library built against an older ABI must be diagnosed, not crashed on.
/// The version is read out of the loaded `.so`, so reporting it after the
/// library is unmapped turns the one diagnostic that exists for this into a
/// segfault — which is exactly how the bump to ABI 2 presented.
#[test]
fn a_library_built_against_an_older_abi_is_diagnosed() {
    let root = scratch("staleabi");
    let home = root.join("home");
    let kit = home.join(".kiln/kits/stale");
    write_kit(&kit, "stale", "0.1.0");
    // The one difference from a kit that works: a version that is not ours.
    let libinfo = kit.join("stale_libinfo.c");
    let src = std::fs::read_to_string(&libinfo).expect("read libinfo");
    std::fs::write(&libinfo, src.replace("KILN_ABI_VERSION,", "1,")).expect("write libinfo");

    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["commands", "--use", "stale"])
        .current_dir(&root)
        .env("HOME", &home)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln");

    assert!(!out.status.success(), "a stale ABI must not be accepted");
    assert!(
        out.status.code().is_some(),
        "kiln died on a signal instead of reporting the mismatch: {}",
        out.status
    );
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(
        msg.contains("ABI version 1"),
        "the diagnostic must name the version it found, got: {msg}"
    );
}
