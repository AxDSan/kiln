//! The combined 0.9.0 proof: `examples/sugar09_tour.kiln` uses the whole
//! milestone's sugar at once — block and raw text, collection literals,
//! slicing, the dot call, `let` inference, the value `if`, `enum`, `match`,
//! `repeat`, `assert`, parameter defaults, named arguments, record literals
//! and updates, optionals with `otherwise`/`if some`/`none`, a list built by a
//! loop, `check` and `defer` — and prints a fixed transcript. Building the
//! tracked example rather than an inline copy keeps the example honest: if any
//! stage's sugar regresses, the shipped tour fails here.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// What the tour prints, in order — the source's own `#` comments say why.
/// Nothing here depends on a dictionary's order: the one loop over a
/// dictionary sorts what it picked before printing it.
const EXPECTED: &[&str] = &[
    "Kiln 0.9.0",
    "the shorthands, in one program",
    r"C:\logs\{today}.txt",
    "Grace, Alan",
    "ADA LOVELACE",
    "4 names",
    "3",
    "warning",
    "10",
    "20",
    "example.com:80, giving up after 5000ms",
    "example.com:8080, giving up after 5000ms",
    "localhost:80, giving up after 250ms",
    "origin at 0,0",
    "moved at 3,0",
    "36",
    "0",
    "ada is 36",
    "Edsger",
    "45",
    "0",
    "2, 4, 6",
    "grace",
    "tick tick tick open close open work close",
];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kiln_sugar09_tour_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Build the tracked example with `extra` flags, and answer where it landed.
fn build(dir: &Path, extra: &[&str]) -> PathBuf {
    let bin = dir.join("sugar09_tour");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .arg("build")
        .arg(repo().join("examples/sugar09_tour.kiln"))
        .args(extra)
        .arg("-o")
        .arg(&bin)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        out.status.success(),
        "the tour failed to build with {extra:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

fn lines_of(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn sugar09_tour_example_prints_its_transcript() {
    let bin = build(&scratch("run"), &[]);
    let run = Command::new(&bin).output().expect("run the tour");
    assert!(
        run.status.success(),
        "the tour exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(lines_of(&run.stdout), EXPECTED, "the transcript did not match");
}

/// A release build compiles the tour's `assert` out. That must change nothing
/// a reader can see, so the same transcript is the check.
#[test]
fn a_release_build_of_the_tour_prints_the_same_transcript() {
    let bin = build(&scratch("release"), &["--release"]);
    let run = Command::new(&bin).output().expect("run the release tour");
    assert!(
        run.status.success(),
        "the release tour exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        lines_of(&run.stdout),
        EXPECTED,
        "the release transcript did not match the debug one"
    );
}

// --- The same tour, cross-built for Windows and run under wine ---------------

fn on_path(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn sugar09_tour_cross_builds_and_runs_under_wine() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("mingw is not installed; skipping the Windows tour test");
        return;
    }
    let dir = scratch("windows");
    build(&dir, &["--os", "windows"]);
    let image = dir.join("sugar09_tour.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());

    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return;
    }
    let out = Command::new("wine")
        .arg(&image)
        .current_dir(&dir)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the tour exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `str::lines` drops the `\r` of a CRLF, which is the only difference a
    // Windows console makes to this transcript.
    assert_eq!(
        lines_of(&out.stdout),
        EXPECTED,
        "the Windows transcript did not match the Linux one"
    );
}
