//! `kiln install`, which has to actually produce a working toolchain.
//!
//! The interesting property is not that files were copied — it is that the
//! copy still *works*: `kiln` finds its runtime by walking up from its own
//! executable, and the name on `PATH` is a symlink into `<prefix>/lib/kiln`.
//! If `current_exe` ever stopped resolving that symlink, every test here that
//! only counted files would still pass and the install would be broken. So
//! the install is used to build and run a program.

use std::path::PathBuf;
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_install_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn kiln(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(args)
        .current_dir(repo())
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln")
}

/// A dry run writes nothing at all. That is the whole promise of the flag, and
/// the only way to check it is to look at the filesystem afterwards.
#[test]
fn a_dry_run_writes_nothing() {
    let dir = scratch("dry");
    let prefix = dir.join("prefix");
    let out = kiln(&["install", "--dry-run", "--prefix", prefix.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("runtime/"), "it did not say what it would copy:\n{text}");
    assert!(
        !prefix.exists(),
        "a dry run created {} — it must write nothing",
        prefix.display()
    );
}

/// The real thing, used from outside the repository: the point of installing.
#[test]
fn an_installed_tree_builds_and_runs_a_program() {
    let dir = scratch("real");
    let prefix = dir.join("prefix");
    let out = kiln(&["install", "--prefix", prefix.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let bin = prefix.join("bin").join("kiln");
    assert!(bin.exists(), "no kiln in {}", prefix.join("bin").display());

    // The name on PATH is a symlink into lib/, not a second copy.
    let meta = std::fs::symlink_metadata(&bin).expect("stat the installed kiln");
    #[cfg(unix)]
    assert!(meta.file_type().is_symlink(), "{} is not a symlink", bin.display());
    #[cfg(not(unix))]
    let _ = meta;

    // Run it from somewhere that is neither the repo nor the prefix, with no
    // KILN_RUNTIME_DIR: everything it needs it must find for itself.
    let src = dir.join("hello.kiln");
    std::fs::write(
        &src,
        "module hello\ntarget console\n\nsub main\n  call print_text(\"installed\")\nend\n",
    )
    .expect("write source");
    let app = dir.join("hello");

    let build = Command::new(&bin)
        .args(["build", src.to_str().unwrap(), "-o", app.to_str().unwrap()])
        .current_dir(&dir)
        .env_remove("KILN_RUNTIME_DIR")
        .output()
        .expect("run the installed kiln");
    assert!(
        build.status.success(),
        "the installed kiln could not build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&app).output().expect("run the built program");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "installed");
}

/// Installing over an existing install needs `--force`, so a second run
/// cannot quietly half-replace a tree someone is using.
#[test]
fn replacing_an_install_needs_force() {
    let dir = scratch("force");
    let prefix = dir.join("prefix");
    assert!(kiln(&["install", "--prefix", prefix.to_str().unwrap()]).status.success());

    let again = kiln(&["install", "--prefix", prefix.to_str().unwrap()]);
    assert!(!again.status.success(), "a second install should have refused");
    let err = String::from_utf8_lossy(&again.stderr);
    assert!(err.contains("--force"), "the refusal did not name the way out:\n{err}");

    let forced = kiln(&["install", "--force", "--prefix", prefix.to_str().unwrap()]);
    assert!(
        forced.status.success(),
        "--force did not replace it:\n{}",
        String::from_utf8_lossy(&forced.stderr)
    );
}

/// A prefix inside the tree being installed is a copy of a directory into its
/// own child. Refused by name rather than discovered as a full disk.
#[test]
fn installing_a_tree_into_itself_is_refused() {
    let inside = repo().join("dist").join("self-install-test");
    let out = kiln(&["install", "--prefix", inside.to_str().unwrap()]);
    assert!(!out.status.success(), "it installed into its own tree");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("inside the tree"), "{err}");
    assert!(!inside.exists(), "it created {} anyway", inside.display());
}
