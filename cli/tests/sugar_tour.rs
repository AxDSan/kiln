//! The combined 0.8.0 proof: `examples/sugar_tour.kiln` uses every shorthand at
//! once — compound assignment, text `+`/`*`, interpolation, a range loop, a
//! `for each` over a dictionary, `in`/`not in`, and a one-line `if` — and prints
//! a fixed transcript. Building the tracked example (not an inline copy) keeps
//! the example honest: if a stage's sugar regresses, the shipped tour fails
//! here. The output is deterministic by construction — the dictionary loop only
//! sums values, so its unspecified key order cannot change the total.
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// What the tour prints, in order — the source's own `#` comments spell out why.
const EXPECTED: &[&str] = &[
    "25",
    "==========",
    "hello, Kiln",
    "15",
    "line 1",
    "line 2",
    "line 3",
    "15",
    "cup is stocked",
    "no hat",
    "4",
];

#[test]
fn sugar_tour_example_prints_its_transcript() {
    let dir = std::env::temp_dir().join("kiln_sugar_tour_run");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let bin = dir.join("sugar_tour");
    let out = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build"])
        .arg(repo().join("examples/sugar_tour.kiln"))
        .arg("-o")
        .arg(&bin)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .output()
        .expect("run kiln build");
    assert!(
        out.status.success(),
        "the tour failed to build:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin).output().expect("run the tour");
    assert!(
        run.status.success(),
        "the tour exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, EXPECTED, "the tour transcript did not match");
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

fn wine_lines(image: &Path, cwd: &Path) -> Option<Vec<String>> {
    if !on_path("wine") {
        eprintln!("wine is not installed; the Windows image was built but not run");
        return None;
    }
    let out = Command::new("wine")
        .arg(image)
        .current_dir(cwd)
        .env("WINEDEBUG", "-all")
        .output()
        .expect("run wine");
    assert!(
        out.status.success(),
        "the tour exited non-zero under wine:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

#[test]
fn sugar_tour_cross_builds_and_runs_under_wine() {
    if !on_path("x86_64-w64-mingw32-gcc") {
        eprintln!("mingw is not installed; skipping the Windows tour test");
        return;
    }
    let dir = std::env::temp_dir().join("kiln_sugar_tour_windows");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let out = dir.join("sugar_tour");
    let status = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["build"])
        .arg(repo().join("examples/sugar_tour.kiln"))
        .args(["--os", "windows", "-o"])
        .arg(&out)
        .env("KILN_RUNTIME_DIR", repo().join("runtime"))
        .status()
        .expect("run kiln build --os windows");
    assert!(status.success(), "kiln build --os windows failed");
    let image = dir.join("sugar_tour.exe");
    assert!(image.is_file(), "expected {} to be written", image.display());

    if let Some(lines) = wine_lines(&image, &dir) {
        assert_eq!(lines, EXPECTED, "the Windows tour transcript did not match");
    }
}
